# Amazon SES sends one kind of email: the link that confirms a removal request from /rights
# (Spec 08, Rights lifecycle). The domain identity signs with Easy DKIM; its three CNAMEs go in
# at Cloudflare by hand (output ses_dkim_records_for_cloudflare), DNS-only. A new account's SES
# starts in the sandbox, which delivers only to verified addresses: request production access
# in the SES console before the form goes live (Spec 11, PB1.5; runbook §12, "Email").

resource "aws_sesv2_email_identity" "mail" {
  email_identity = var.mail_domain

  dkim_signing_attributes {
    next_signing_key_length = "RSA_2048_BIT"
  }
}

# A custom MAIL FROM domain, so the envelope sender is mail.<domain> rather than amazonses.com
# and SPF aligns with the From domain for DMARC as well as DKIM. Its MX and TXT records go in at
# Cloudflare by hand (output ses_mail_from_records_for_cloudflare). Until they resolve, SES keeps
# sending from its own MAIL FROM domain, so DKIM alone carries DMARC.
resource "aws_sesv2_email_identity_mail_from_attributes" "mail" {
  email_identity         = aws_sesv2_email_identity.mail.email_identity
  mail_from_domain       = "${var.mail_from_subdomain}.${var.mail_domain}"
  behavior_on_mx_failure = "USE_DEFAULT_VALUE"
}

# Bounces and complaints (Spec 11, PB1.5). SES publishes each one to this topic, which emails
# the owner; the runbook says how to read them. SES also adds every hard bounce and complaint to
# the account's suppression list, so a bad address is not mailed again. The topic is not
# KMS-encrypted: SES would need a customer key policy to publish to an encrypted topic, and
# the notices carry only addresses and SMTP diagnostics, never a teacher's words.
resource "aws_sns_topic" "ses_feedback" {
  name = "wt-ses-feedback"
}

data "aws_iam_policy_document" "ses_feedback" {
  statement {
    sid       = "SesPublishesFeedback"
    actions   = ["sns:Publish"]
    resources = [aws_sns_topic.ses_feedback.arn]
    principals {
      type        = "Service"
      identifiers = ["ses.amazonaws.com"]
    }
    condition {
      test     = "StringEquals"
      variable = "aws:SourceAccount"
      values   = [data.aws_caller_identity.current.account_id]
    }
    condition {
      test     = "ArnLike"
      variable = "aws:SourceArn"
      values   = [aws_sesv2_email_identity.mail.arn]
    }
  }
}

resource "aws_sns_topic_policy" "ses_feedback" {
  arn    = aws_sns_topic.ses_feedback.arn
  policy = data.aws_iam_policy_document.ses_feedback.json
}

# AWS emails a confirmation link to this address; nothing arrives until it is clicked.
resource "aws_sns_topic_subscription" "ses_feedback_email" {
  topic_arn = aws_sns_topic.ses_feedback.arn
  protocol  = "email"
  endpoint  = var.alert_email
}

resource "aws_ses_identity_notification_topic" "feedback" {
  for_each = toset(["Bounce", "Complaint"])

  identity                 = aws_sesv2_email_identity.mail.email_identity
  notification_type        = each.key
  topic_arn                = aws_sns_topic.ses_feedback.arn
  include_original_headers = true

  depends_on = [aws_sns_topic_policy.ses_feedback]
}
