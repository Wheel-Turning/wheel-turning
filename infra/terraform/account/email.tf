# Amazon SES sends one kind of email: the link that confirms a removal request from /rights
# (Spec 08, Rights lifecycle). The domain identity signs with Easy DKIM; its three CNAMEs go in
# at Cloudflare by hand (output ses_dkim_records_for_cloudflare), DNS-only. A new account's SES
# starts in the sandbox, which delivers only to verified addresses: request production access
# in the SES console before the form goes live.

resource "aws_sesv2_email_identity" "mail" {
  email_identity = var.mail_domain

  dkim_signing_attributes {
    next_signing_key_length = "RSA_2048_BIT"
  }
}
