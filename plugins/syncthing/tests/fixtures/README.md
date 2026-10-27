# Public TLS test fixtures

These certificates and private key are deliberately public test data. Never use
them in an installation or trust store. They exercise a localhost rustls server
without external programs or a certificate-generation dependency during tests.

The certificate names deliberately differ from localhost: the plugin authenticates
the exact certificate selected from the local Syncthing installation and verifies
the TLS handshake signature. `wrong-cert.pem` has the same key but a different
certificate identity, proving that key possession alone cannot bypass the pin.

Generated using OpenSSL:

```sh
openssl req -x509 -newkey rsa:2048 -keyout localhost-key.pem -out localhost-cert.pem -days 3650 -noenc -subj '/CN=explorie-test.invalid'
openssl x509 -in localhost-cert.pem -signkey localhost-key.pem -set_serial 2 -days 3650 -out wrong-cert.pem
```
