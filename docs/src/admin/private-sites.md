# Private sites

To keep everything behind a login, take **View posts** away from the
Anonymous role (**Admin → Roles**). Then:

- visitors are sent to the login page;
- the API answers `401` to requests without a key;
- file links on pages carry a signature that expires after an hour or two,
  so files can't be fetched by guessing or sharing their URLs.

Pick a [registration mode](../install/first-steps.md#who-can-register) to
match: usually `invite`, `approval` or `closed`.

## Files need to go through the app

Signed links only protect files the app serves itself. Leave
`storage.public_base_url` unset: with local storage the app serves files
under `/data/`, and with S3 it streams them from the bucket, so the bucket
can stay private.

A CDN or public bucket address would hand files to anyone who has the
link, so the server warns about this combination at startup.
