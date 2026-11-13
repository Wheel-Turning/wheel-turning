# The account already has the AWS-created Default-Services-Monitor, and an account allows only one
# AWS services monitor, so this stack subscribes to it instead of creating its own.
existing_anomaly_monitor_arn = "arn:aws:ce::<AWS_ACCOUNT_ID>:anomalymonitor/<ANOMALY_MONITOR_ID>"
