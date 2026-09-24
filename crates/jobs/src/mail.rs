//! Outgoing mail over SMTP, and the `mail.send` job that sends it, so a
//! slow mail server never holds up a request.

use std::sync::Arc;
use std::time::Duration;

use lettre::message::Mailbox;
use lettre::message::header::ContentType;
use lettre::transport::smtp::authentication::Credentials;
use lettre::{AsyncSmtpTransport, AsyncTransport, Message, Tokio1Executor};
use moekura_core::config::{MailConfig, MailTls};
use moekura_core::jobs::SendMail;

use crate::{JobError, Registry};

#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error("`{address}` is not an email address: {error}")]
    Address {
        address: String,
        error: lettre::address::AddressError,
    },
    #[error("could not build the message: {0}")]
    Message(#[from] lettre::error::Error),
    #[error("{0}")]
    Smtp(#[from] lettre::transport::smtp::Error),
}

impl MailError {
    /// Whether trying again later could help: the server was unreachable
    /// or said to try later, rather than refusing the message.
    pub fn is_transient(&self) -> bool {
        match self {
            MailError::Smtp(e) => !e.is_permanent(),
            MailError::Address { .. } | MailError::Message(_) => false,
        }
    }
}

/// Sends mail through the configured SMTP server.
#[derive(Clone)]
pub struct Mailer {
    transport: AsyncSmtpTransport<Tokio1Executor>,
    from: Mailbox,
}

impl Mailer {
    /// A mailer for `config`, which must have mail enabled
    /// ([`MailConfig::is_enabled`]). Doesn't connect yet.
    pub fn new(config: &MailConfig) -> Result<Self, MailError> {
        let from = parse_mailbox(&config.from)?;
        let builder = match config.tls {
            MailTls::Starttls => {
                AsyncSmtpTransport::<Tokio1Executor>::starttls_relay(&config.host)?
            }
            MailTls::Tls => AsyncSmtpTransport::<Tokio1Executor>::relay(&config.host)?,
            MailTls::None => AsyncSmtpTransport::<Tokio1Executor>::builder_dangerous(&config.host),
        };
        let mut builder = builder
            .port(config.port_or_default())
            .timeout(Some(Duration::from_secs(config.timeout_secs)));
        if !config.username.is_empty() {
            builder = builder.credentials(Credentials::new(
                config.username.clone(),
                config.password.clone(),
            ));
        }
        Ok(Self {
            transport: builder.build(),
            from,
        })
    }

    /// Sends a plain-text message to `to`.
    pub async fn send(&self, to: &str, subject: &str, body: &str) -> Result<(), MailError> {
        let message = Message::builder()
            .from(self.from.clone())
            .to(parse_mailbox(to)?)
            .subject(subject)
            .header(ContentType::TEXT_PLAIN)
            .body(body.to_owned())?;
        self.transport.send(message).await?;
        Ok(())
    }
}

fn parse_mailbox(address: &str) -> Result<Mailbox, MailError> {
    address.parse().map_err(|error| MailError::Address {
        address: address.to_owned(),
        error,
    })
}

/// Handles `mail.send`. Without a mailer (mail isn't configured), queued
/// messages fail for good rather than waiting forever.
#[derive(Clone)]
pub struct MailJobs {
    pub mailer: Option<Arc<Mailer>>,
}

impl MailJobs {
    pub fn register(self, registry: &mut Registry) {
        registry.register(move |job: SendMail| {
            let jobs = self.clone();
            async move { jobs.send(job).await }
        });
    }

    async fn send(&self, job: SendMail) -> Result<(), JobError> {
        let Some(mailer) = &self.mailer else {
            return Err(JobError::permanent("mail is not configured (mail.host)"));
        };
        match mailer.send(&job.to, &job.subject, &job.body).await {
            Ok(()) => {
                tracing::info!(subject = job.subject, "mail sent");
                Ok(())
            }
            Err(error) if error.is_transient() => Err(JobError::retry(error)),
            Err(error) => Err(JobError::permanent(error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;
    use std::sync::Mutex;

    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    use tokio::net::TcpListener;

    use super::*;

    /// A tiny SMTP server that accepts everything except recipients at
    /// `refused.example`, and keeps what it receives.
    struct FakeSmtp {
        addr: SocketAddr,
        received: Arc<Mutex<Vec<String>>>,
    }

    impl FakeSmtp {
        async fn start() -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let received = Arc::new(Mutex::new(Vec::new()));
            let store = received.clone();
            tokio::spawn(async move {
                while let Ok((socket, _)) = listener.accept().await {
                    let store = store.clone();
                    tokio::spawn(async move {
                        let (read, mut write) = socket.into_split();
                        let mut lines = BufReader::new(read).lines();
                        write.write_all(b"220 fake ESMTP\r\n").await.unwrap();
                        let mut data: Option<String> = None;
                        while let Ok(Some(line)) = lines.next_line().await {
                            if let Some(message) = &mut data {
                                if line == "." {
                                    store.lock().unwrap().push(data.take().unwrap());
                                    write.write_all(b"250 queued\r\n").await.unwrap();
                                } else {
                                    message.push_str(&line);
                                    message.push('\n');
                                }
                                continue;
                            }
                            let reply: &[u8] = match line.get(..4).map(str::to_ascii_uppercase) {
                                Some(verb) if verb == "EHLO" || verb == "HELO" => b"250 fake\r\n",
                                Some(verb)
                                    if verb == "RCPT" && line.contains("refused.example") =>
                                {
                                    b"550 no such user\r\n"
                                }
                                Some(verb) if verb == "DATA" => {
                                    data = Some(String::new());
                                    b"354 go ahead\r\n"
                                }
                                Some(verb) if verb == "QUIT" => {
                                    let _ = write.write_all(b"221 bye\r\n").await;
                                    break;
                                }
                                _ => b"250 ok\r\n",
                            };
                            write.write_all(reply).await.unwrap();
                        }
                    });
                }
            });
            Self { addr, received }
        }

        fn config(&self) -> MailConfig {
            MailConfig {
                host: self.addr.ip().to_string(),
                port: Some(self.addr.port()),
                tls: MailTls::None,
                from: "Moekura <noreply@example.com>".into(),
                timeout_secs: 5,
                ..MailConfig::default()
            }
        }
    }

    fn job(to: &str) -> SendMail {
        SendMail {
            to: to.into(),
            subject: "Hello".into(),
            body: "Just testing.".into(),
        }
    }

    #[tokio::test]
    async fn sends_and_sorts_failures() {
        let server = FakeSmtp::start().await;
        let jobs = MailJobs {
            mailer: Some(Arc::new(Mailer::new(&server.config()).unwrap())),
        };
        jobs.send(job("someone@example.com")).await.unwrap();
        let received = server.received.lock().unwrap().clone();
        assert_eq!(received.len(), 1);
        assert!(received[0].contains("Subject: Hello"), "{}", received[0]);
        assert!(received[0].contains("To: someone@example.com"));
        assert!(received[0].contains("Just testing."));

        assert!(matches!(
            jobs.send(job("someone@refused.example")).await,
            Err(JobError::Permanent(_))
        ));
        assert!(matches!(
            jobs.send(job("not an address")).await,
            Err(JobError::Permanent(_))
        ));

        // Nobody listening: worth retrying.
        let mut config = server.config();
        config.port = Some(1);
        let unreachable = MailJobs {
            mailer: Some(Arc::new(Mailer::new(&config).unwrap())),
        };
        assert!(matches!(
            unreachable.send(job("someone@example.com")).await,
            Err(JobError::Retry(_))
        ));

        let off = MailJobs { mailer: None };
        assert!(matches!(
            off.send(job("someone@example.com")).await,
            Err(JobError::Permanent(_))
        ));
    }

    #[test]
    fn needs_a_valid_sender() {
        let config = MailConfig {
            host: "localhost".into(),
            from: "nobody".into(),
            ..MailConfig::default()
        };
        assert!(matches!(
            Mailer::new(&config),
            Err(MailError::Address { .. })
        ));
    }
}
