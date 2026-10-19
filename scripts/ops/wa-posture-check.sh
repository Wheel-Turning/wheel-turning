#!/usr/bin/env bash
# Read-only check of the live AWS account against the controls the Well-Architected review relies
# on (docs/well-architected/README.md). It changes nothing. Run from Git Bash after
# `aws sso login --profile wheel-turning`:
#
#   AWS_PROFILE=wheel-turning scripts/ops/wa-posture-check.sh
#
# Each line is PASS, WARN or FAIL with the control and the WA best practice. It exits 1 when any
# line is FAIL, so it can run before a release (runbook §9). The offline twin of these checks is
# the modules' guardrails.tftest.hcl files, which CI runs on every change.
set -uo pipefail

NAME=${WT_NAME:-wt-v1}
REGION=${AWS_REGION:-us-east-2}
# MSYS_NO_PATHCONV: Git Bash would otherwise turn /wt-v1/web into a Windows path.
export AWS_REGION=$REGION AWS_PAGER="" MSYS_NO_PATHCONV=1
ACCOUNT=$(aws sts get-caller-identity --query Account --output text) || {
  echo "FAIL  no AWS credentials: run aws sso login --profile wheel-turning first"
  exit 1
}
FAILS=0

pass() { printf 'PASS  %-58s %s\n' "$1" "$2"; }
warn() { printf 'WARN  %-58s %s\n' "$1" "$2"; }
fail() { printf 'FAIL  %-58s %s\n' "$1" "$2"; FAILS=$((FAILS + 1)); }
# check <label> <bp> <expected> <actual>: PASS when equal, FAIL otherwise.
check() { if [ "$4" = "$3" ]; then pass "$1" "$2"; else fail "$1 (got: ${4:-nothing})" "$2"; fi; }
q() { aws "$@" --output text 2>/dev/null | tr -d '\r'; }

echo "Account $ACCOUNT, region $REGION, environment $NAME"

# ---- Detection and audit -------------------------------------------------------------------
detector=$(q guardduty list-detectors --query 'DetectorIds[0]')
if [ -n "$detector" ] && [ "$detector" != "None" ]; then
  check "GuardDuty detector enabled" SEC04-BP03 ENABLED "$(q guardduty get-detector --detector-id "$detector" --query Status)"
else
  fail "GuardDuty detector exists" SEC04-BP03
fi
analyzers=$(q accessanalyzer list-analyzers --query 'length(analyzers[?status==`ACTIVE`])')
if [ "${analyzers:-0}" -ge 2 ]; then pass "IAM Access Analyzer: external and unused access" SEC03-BP04; else fail "IAM Access Analyzer analyzers active (got ${analyzers:-0}, want 2)" SEC03-BP04; fi
check "CloudTrail wt-account is logging" SEC04-BP01 True "$(q cloudtrail get-trail-status --name wt-account --query IsLogging)"
check "CloudTrail log file validation" SEC04-BP01 True "$(q cloudtrail describe-trails --trail-name-list wt-account --query 'trailList[0].LogFileValidationEnabled')"
check "CloudTrail is multi-region" SEC04-BP01 True "$(q cloudtrail describe-trails --trail-name-list wt-account --query 'trailList[0].IsMultiRegionTrail')"

# ---- Identity -------------------------------------------------------------------------------
check "Account S3 public access block" SEC03-BP07 "True	True	True	True" \
  "$(q s3control get-public-access-block --account-id "$ACCOUNT" --query 'PublicAccessBlockConfiguration.[BlockPublicAcls,IgnorePublicAcls,BlockPublicPolicy,RestrictPublicBuckets]')"
check "Root user has MFA" SEC01-BP02 1 "$(q iam get-account-summary --query 'SummaryMap.AccountMFAEnabled')"
check "Root user has no access keys" SEC01-BP02 0 "$(q iam get-account-summary --query 'SummaryMap.AccountAccessKeysPresent')"
# Long-lived keys: the review's M3 (wt-backup) retires with PB1.2; any other is a finding.
cutoff=$(date -u -d '90 days ago' +%Y-%m-%dT%H:%M:%S 2>/dev/null || echo 1970-01-01T00:00:00)
for user in $(q iam list-users --query 'Users[].UserName'); do
  for created in $(q iam list-access-keys --user-name "$user" --query 'AccessKeyMetadata[?Status==`Active`].CreateDate'); do
    if [[ "$created" < "$cutoff" ]]; then fail "IAM user $user has an active key older than 90 days" SEC02-BP02;
    else warn "IAM user $user has an active access key" SEC02-BP02; fi
  done
done

# ---- Secrets --------------------------------------------------------------------------------
for secret in $(q secretsmanager list-secrets --filters Key=name,Values="$NAME/" --query 'SecretList[].Name'); do
  changed=$(q secretsmanager describe-secret --secret-id "$secret" --query 'LastChangedDate')
  if [[ "${changed%%.*}" < "$cutoff" ]]; then warn "Secret $secret unchanged for 90 days" SEC02-BP05; fi
done

# ---- Database -------------------------------------------------------------------------------
db() { q rds describe-db-instances --db-instance-identifier "$NAME" --query "DBInstances[0].$1"; }
check "RDS storage encrypted" SEC08-BP02 True "$(db StorageEncrypted)"
check "RDS not publicly accessible" SEC05-BP01 False "$(db PubliclyAccessible)"
check "RDS deletion protection" REL09-BP02 True "$(db DeletionProtection)"
retention=$(db BackupRetentionPeriod)
if [ "${retention:-0}" -ge 7 ]; then pass "RDS backups kept $retention days" REL09-BP03; else fail "RDS backup retention ${retention:-0} days, want 7 or more" REL09-BP03; fi
max=$(db MaxAllocatedStorage)
if [ -n "$max" ] && [ "$max" != "None" ]; then pass "RDS storage autoscaling to $max GB" SUS04-BP04; else warn "RDS storage autoscaling off (UB3.9, PR 215)" SUS04-BP04; fi
if [ "$(db MultiAZ)" = "True" ]; then pass "RDS Multi-AZ" REL10-BP01; else warn "RDS single-AZ (accepted for the private beta; UB3.3)" REL10-BP01; fi
latest=$(q rds describe-db-instances --db-instance-identifier "$NAME" --query 'DBInstances[0].LatestRestorableTime')
[ -n "$latest" ] && [ "$latest" != "None" ] && pass "RDS point-in-time restore to $latest" REL09-BP01

# ---- Storage --------------------------------------------------------------------------------
for b in transcripts scratch models alb-logs; do
  bucket="$NAME-$b-$ACCOUNT"
  if ! aws s3api head-bucket --bucket "$bucket" >/dev/null 2>&1; then
    [ "$b" = alb-logs ] && warn "Bucket $bucket missing (UB3.9, PR 215)" SEC04-BP01 || fail "Bucket $bucket missing" SEC08-BP02
    continue
  fi
  check "Bucket $b blocks public access" SEC03-BP07 "True	True	True	True" \
    "$(q s3api get-public-access-block --bucket "$bucket" --query 'PublicAccessBlockConfiguration.[BlockPublicAcls,IgnorePublicAcls,BlockPublicPolicy,RestrictPublicBuckets]')"
  enc=$(q s3api get-bucket-encryption --bucket "$bucket" --query 'ServerSideEncryptionConfiguration.Rules[0].ApplyServerSideEncryptionByDefault.SSEAlgorithm')
  if [ -n "$enc" ]; then pass "Bucket $b encrypted ($enc)" SEC08-BP02; else fail "Bucket $b has no default encryption" SEC08-BP02; fi
  if aws s3api get-bucket-policy --bucket "$bucket" --query Policy --output text 2>/dev/null | grep -q 'aws:SecureTransport'; then
    pass "Bucket $b denies requests without TLS" SEC09-BP02
  else fail "Bucket $b policy does not deny plain HTTP" SEC09-BP02; fi
done
objects=$(q s3api list-objects-v2 --bucket "$NAME-scratch-$ACCOUNT" --query 'KeyCount')
if [ "${objects:-None}" = None ] || [ "$objects" = 0 ]; then pass "Scratch bucket empty" SEC07-BP04; else warn "Scratch bucket holds $objects objects (expire in a day; audio must not persist)" SEC07-BP04; fi

# ---- Network and edge -----------------------------------------------------------------------
for sg in web batch; do
  open=$(q ec2 describe-security-groups --filters Name=group-name,Values="$NAME-$sg" \
    --query "SecurityGroups[0].IpPermissionsEgress[?IpProtocol=='-1'] | length(@)")
  if [ "${open:-0}" = 0 ]; then pass "Security group $sg: egress scoped by port" SEC05-BP02;
  else warn "Security group $sg allows all protocols out (UB3.9, PR 215)" SEC05-BP02; fi
done
lb=$(q elbv2 describe-load-balancers --names "$NAME" --query 'LoadBalancers[0].LoadBalancerArn')
if [ -n "$lb" ] && [ "$lb" != "None" ]; then
  attr() { q elbv2 describe-load-balancer-attributes --load-balancer-arn "$lb" --query "Attributes[?Key=='$1'].Value | [0]"; }
  check "Load balancer drops invalid header fields" SEC05-BP02 true "$(attr routing.http.drop_invalid_header_fields.enabled)"
  if [ "$(attr access_logs.s3.enabled)" = true ]; then pass "Load balancer access logs on" SEC04-BP01; else warn "Load balancer access logs off (UB3.9, PR 215)" SEC04-BP01; fi
  # Stage C of the edge cutover (runbook §12, The edge) removes the public IPv4 addresses.
  iptype=$(q elbv2 describe-load-balancers --names "$NAME" --query 'LoadBalancers[0].IpAddressType')
  if [ "$iptype" = dualstack-without-public-ipv4 ]; then pass "Load balancer has no public IPv4 (Cloudflare only)" SEC05-BP01;
  else warn "Load balancer is $iptype: edge cutover stage C not applied" SEC05-BP01; fi
  for policy in $(q elbv2 describe-listeners --load-balancer-arn "$lb" --query 'Listeners[?Protocol==`HTTPS`].SslPolicy'); do
    case "$policy" in *TLS13*) pass "HTTPS listener policy $policy" SEC09-BP02 ;; *) warn "HTTPS listener policy $policy is not a TLS 1.3 policy" SEC09-BP02 ;; esac
  done
else
  fail "Load balancer $NAME not found" REL11-BP01
fi
check "ECR scans images on push" SEC06-BP01 True "$(q ecr describe-repositories --repository-names "$NAME" --query 'repositories[0].imageScanningConfiguration.scanOnPush')"
check "ECR tags immutable" REL08-BP04 IMMUTABLE "$(q ecr describe-repositories --repository-names "$NAME" --query 'repositories[0].imageTagMutability')"

# ---- Service and observability ---------------------------------------------------------------
svc() { q ecs describe-services --cluster "$NAME" --services "$NAME-web" --query "services[0].$1"; }
check "ECS circuit breaker with rollback" OPS06-BP04 "True	True" "$(svc 'deploymentConfiguration.deploymentCircuitBreaker.[enable,rollback]')"
running=$(svc runningCount)
if [ "${running:-0}" -ge 1 ]; then pass "Web service running $running task(s)" REL11-BP01; else warn "Web service has no running task (scaled to zero?)" REL11-BP01; fi
alarming=$(q cloudwatch describe-alarms --alarm-name-prefix "$NAME-" --state-value ALARM --query 'MetricAlarms[].AlarmName')
if [ -z "$alarming" ]; then pass "No $NAME alarm in ALARM" OPS08-BP04; else warn "In ALARM: $alarming" OPS08-BP04; fi
topic=$(q sns list-topics --query "Topics[?ends_with(TopicArn, ':$NAME-alarms')].TopicArn | [0]")
if [ -n "$topic" ] && [ "$topic" != "None" ]; then
  confirmed=$(q sns get-topic-attributes --topic-arn "$topic" --query 'Attributes.SubscriptionsConfirmed')
  pending=$(q sns get-topic-attributes --topic-arn "$topic" --query 'Attributes.SubscriptionsPending')
  if [ "${confirmed:-0}" -ge 2 ]; then pass "Alarm topic has $confirmed confirmed subscribers" OPS10-BP04;
  elif [ "${confirmed:-0}" -ge 1 ]; then warn "Alarm topic has 1 confirmed subscriber, ${pending:-0} pending (PB4.2 wants two)" OPS10-BP04;
  else fail "Alarm topic has no confirmed subscriber" OPS10-BP04; fi
else
  fail "Alarm topic $NAME-alarms not found" OPS08-BP04
fi
retention=$(q logs describe-log-groups --log-group-name-prefix "/$NAME/web" --query 'logGroups[0].retentionInDays')
if [ -n "$retention" ] && [ "$retention" != "None" ]; then pass "Web logs kept $retention days" SEC04-BP02; else warn "Web log group never expires" COST04-BP05; fi

# ---- Cost -----------------------------------------------------------------------------------
budget=$(q budgets describe-budgets --account-id "$ACCOUNT" --query 'Budgets[?BudgetName==`dharma-gates-monthly`].BudgetLimit.Amount | [0]')
if [ -n "$budget" ] && [ "$budget" != "None" ]; then pass "Monthly budget set ($budget USD)" COST01-BP03; else fail "Monthly budget dharma-gates-monthly missing" COST01-BP03; fi

echo
if [ "$FAILS" -gt 0 ]; then echo "$FAILS check(s) failed"; exit 1; fi
echo "No check failed"
