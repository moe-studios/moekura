//! A software authenticator for tests: answers the options the site
//! sends as a browser with a platform authenticator would, with ES256
//! passkeys that keep a sign count, and `none` attestation.

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use openssl::bn::{BigNum, BigNumContext};
use openssl::ec::{EcGroup, EcKey};
use openssl::ecdsa::EcdsaSig;
use openssl::nid::Nid;
use openssl::pkey::Private;
use openssl::sha::sha256;
use serde_json::{Value, json};

/// User present and verified.
const FLAGS: u8 = 0x01 | 0x04;
/// Attested credential data follows.
const ATTESTED: u8 = 0x40;

pub(crate) struct Credential {
    pub(crate) id: Vec<u8>,
    key: EcKey<Private>,
    user_handle: Vec<u8>,
    /// The sign count it last gave.
    pub(crate) counter: u32,
}

pub(crate) struct Authenticator {
    /// The page's origin, as the browser reports it.
    origin: String,
    pub(crate) credentials: Vec<Credential>,
}

fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

fn unb64(value: &Value) -> Vec<u8> {
    URL_SAFE_NO_PAD
        .decode(value.as_str().expect("a base64url string"))
        .expect("valid base64url")
}

/// A CBOR head: major type `major` with argument `n`.
fn head(major: u8, n: usize) -> Vec<u8> {
    let major = major << 5;
    match n {
        0..24 => vec![major | n as u8],
        24..256 => vec![major | 24, n as u8],
        _ => {
            let mut out = vec![major | 25];
            out.extend_from_slice(&(n as u16).to_be_bytes());
            out
        }
    }
}

fn cbor_bytes(bytes: &[u8]) -> Vec<u8> {
    let mut out = head(2, bytes.len());
    out.extend_from_slice(bytes);
    out
}

fn cbor_text(text: &str) -> Vec<u8> {
    let mut out = head(3, text.len());
    out.extend_from_slice(text.as_bytes());
    out
}

impl Authenticator {
    /// One used on pages at `origin`.
    pub(crate) fn new(origin: &str) -> Self {
        Self {
            origin: origin.to_owned(),
            credentials: Vec::new(),
        }
    }

    fn client_data(&self, kind: &str, challenge: &Value) -> Vec<u8> {
        json!({
            "type": kind,
            "challenge": challenge,
            "origin": self.origin,
            "crossOrigin": false,
        })
        .to_string()
        .into_bytes()
    }

    /// Makes a passkey for the registration `options` the site sent
    /// (`{"publicKey": …}`), and answers as `navigator.credentials.create`
    /// would.
    pub(crate) fn register(&mut self, options: &Value) -> Value {
        let options = &options["publicKey"];
        let rp_id = options["rp"]["id"].as_str().expect("an RP ID");
        let group = EcGroup::from_curve_name(Nid::X9_62_PRIME256V1).unwrap();
        let key = EcKey::generate(&group).unwrap();
        let (mut x, mut y) = (BigNum::new().unwrap(), BigNum::new().unwrap());
        let mut context = BigNumContext::new().unwrap();
        key.public_key()
            .affine_coordinates(&group, &mut x, &mut y, &mut context)
            .unwrap();
        let mut id = vec![0; 16];
        openssl::rand::rand_bytes(&mut id).unwrap();

        // COSE_Key: {1: 2 (EC2), 3: -7 (ES256), -1: 1 (P-256), -2: x, -3: y}
        let mut cose = vec![0xa5, 0x01, 0x02, 0x03, 0x26, 0x20, 0x01, 0x21];
        cose.extend(cbor_bytes(&x.to_vec_padded(32).unwrap()));
        cose.push(0x22);
        cose.extend(cbor_bytes(&y.to_vec_padded(32).unwrap()));

        let mut auth_data = sha256(rp_id.as_bytes()).to_vec();
        auth_data.push(FLAGS | ATTESTED);
        auth_data.extend_from_slice(&0u32.to_be_bytes());
        auth_data.extend_from_slice(&[0; 16]); // AAGUID
        auth_data.extend_from_slice(&(id.len() as u16).to_be_bytes());
        auth_data.extend_from_slice(&id);
        auth_data.extend(cose);

        // {"fmt": "none", "attStmt": {}, "authData": …}
        let mut attestation = vec![0xa3];
        attestation.extend(cbor_text("fmt"));
        attestation.extend(cbor_text("none"));
        attestation.extend(cbor_text("attStmt"));
        attestation.push(0xa0);
        attestation.extend(cbor_text("authData"));
        attestation.extend(cbor_bytes(&auth_data));

        let client_data = self.client_data("webauthn.create", &options["challenge"]);
        self.credentials.push(Credential {
            id: id.clone(),
            key,
            user_handle: unb64(&options["user"]["id"]),
            counter: 0,
        });
        json!({
            "id": b64(&id),
            "rawId": b64(&id),
            "type": "public-key",
            "response": {
                "attestationObject": b64(&attestation),
                "clientDataJSON": b64(&client_data),
                "transports": ["internal"],
            },
            "clientExtensionResults": {},
        })
    }

    /// Signs the login `options` the site sent with passkey `index`, and
    /// answers as `navigator.credentials.get` would.
    pub(crate) fn sign(&mut self, options: &Value, index: usize) -> Value {
        let options = &options["publicKey"];
        let rp_id = options["rpId"].as_str().expect("an RP ID");
        let client_data = self.client_data("webauthn.get", &options["challenge"]);
        let credential = &mut self.credentials[index];
        if let Some(allowed) = options["allowCredentials"]
            .as_array()
            .filter(|a| !a.is_empty())
        {
            assert!(
                allowed.iter().any(|c| unb64(&c["id"]) == credential.id),
                "the site asked for other passkeys"
            );
        }
        credential.counter += 1;
        let mut auth_data = sha256(rp_id.as_bytes()).to_vec();
        auth_data.push(FLAGS);
        auth_data.extend_from_slice(&credential.counter.to_be_bytes());
        let mut signed = auth_data.clone();
        signed.extend_from_slice(&sha256(&client_data));
        let signature = EcdsaSig::sign(&sha256(&signed), &credential.key)
            .unwrap()
            .to_der()
            .unwrap();
        json!({
            "id": b64(&credential.id),
            "rawId": b64(&credential.id),
            "type": "public-key",
            "response": {
                "authenticatorData": b64(&auth_data),
                "clientDataJSON": b64(&client_data),
                "signature": b64(&signature),
                "userHandle": b64(&credential.user_handle),
            },
            "clientExtensionResults": {},
        })
    }
}
