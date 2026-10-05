# Sets up the transcription worker on the owner's Windows PC (Spec 01, GPU access).
#
# Checks that the AWS CLI and the Session Manager plugin are installed, finds the running batch
# host and the database, then opens an SSM port forward to the database in its own PowerShell
# window. The worker connects to localhost:<LocalPort> through that forward. Close the window to
# end the forward, and stop the batch host afterwards (Spec 07 runbook).
#
# Usage:
#   powershell -ExecutionPolicy Bypass -File scripts\worker-setup.ps1
#   powershell -ExecutionPolicy Bypass -File scripts\worker-setup.ps1 -CheckOnly
#
# -CheckOnly checks the installed tools and stops before any AWS call.
param(
    [string]$AwsProfile = 'wheel-turning',
    [string]$Region = 'us-east-2',
    # Terraform's local.name in infra/terraform/envs/v1: the RDS identifier, and the batch host's Name tag prefix.
    [string]$StackName = 'wt-v1',
    # 5432 is left free for the dev database in Docker.
    [int]$LocalPort = 5433,
    [switch]$CheckOnly
)
$ErrorActionPreference = 'Stop'

function Stop-Setup([string]$Message) {
    Write-Host ''
    Write-Host "worker-setup: $Message" -ForegroundColor Red
    exit 1
}

# 1. Tools. The plugin is what `aws ssm start-session` hands the forward to; without it the CLI
#    fails late with a message that does not name the missing piece.
if (-not (Get-Command session-manager-plugin -ErrorAction SilentlyContinue)) {
    Stop-Setup @'
the AWS Session Manager plugin is not installed or not on PATH.
The worker reaches the database through an SSM port forward, which needs the plugin.
Install it, then open a new PowerShell window so PATH picks it up:

    winget install Amazon.SessionManagerPlugin

Check it with: session-manager-plugin --version
'@
}
$pluginVersion = (& session-manager-plugin --version 2>$null | Select-Object -First 1)
Write-Host "Session Manager plugin $pluginVersion"

if (-not (Get-Command aws -ErrorAction SilentlyContinue)) {
    Stop-Setup @'
the AWS CLI v2 is not installed or not on PATH.
Install it, then open a new PowerShell window:

    winget install Amazon.AWSCLI
'@
}
Write-Host ((& aws --version 2>&1) -join ' ')

if ($CheckOnly) {
    Write-Host 'Tools are installed. Stopping before any AWS call (-CheckOnly).'
    exit 0
}

# 2. Sign-in. SSO tokens expire, and the owner signs in, never this script.
$aws = @('--profile', $AwsProfile, '--region', $Region)
& aws sts get-caller-identity @aws --output text --query Account *> $null
if ($LASTEXITCODE -ne 0) {
    Stop-Setup "the AWS profile '$AwsProfile' is not signed in. Run: aws sso login --profile $AwsProfile"
}

# 3. The batch host must be running; the forward goes through it (Spec 01).
# An empty result comes back as no output at all, so join and trim rather than call methods on it.
$instanceId = ((& aws ec2 describe-instances @aws `
        --filters "Name=tag:Name,Values=$StackName-batch" 'Name=instance-state-name,Values=running' `
        --query 'Reservations[].Instances[].InstanceId' --output text) -join ' ').Trim()
if ($LASTEXITCODE -ne 0) {
    Stop-Setup "could not list EC2 instances in $Region with the profile '$AwsProfile'."
}
if (-not $instanceId -or $instanceId -eq 'None') {
    Stop-Setup "no running batch host tagged $StackName-batch. Start it with batch_host_enabled = true (Spec 01) and run this again."
}
if ($instanceId -match '\s') {
    Stop-Setup "more than one running instance is tagged $StackName-batch ($instanceId). Stop the extra one first."
}

$dbHost = ((& aws rds describe-db-instances @aws --db-instance-identifier $StackName `
        --query 'DBInstances[0].Endpoint.Address' --output text) -join ' ').Trim()
if ($LASTEXITCODE -ne 0 -or -not $dbHost -or $dbHost -eq 'None') {
    Stop-Setup "could not find the database $StackName in $Region."
}

# 4. The local port must be free.
if (Get-NetTCPConnection -LocalPort $LocalPort -State Listen -ErrorAction SilentlyContinue) {
    Stop-Setup "local port $LocalPort is already in use. Close what holds it, or pass -LocalPort with another port."
}

# 5. Open the forward in its own PowerShell window, so it stays up while the worker runs here.
$forward = "aws ssm start-session --profile $AwsProfile --region $Region --target $instanceId " +
    '--document-name AWS-StartPortForwardingSessionToRemoteHost ' +
    "--parameters host=$dbHost,portNumber=5432,localPortNumber=$LocalPort"
Write-Host "Opening the port forward: localhost:$LocalPort -> $dbHost`:5432 via $instanceId"
Start-Process powershell.exe -ArgumentList @(
    '-NoExit', '-NoProfile', '-Command',
    "`$Host.UI.RawUI.WindowTitle = 'Wheel Turning port forward (close to stop)'; $forward"
)

# 6. Wait for the forward to listen.
$deadline = (Get-Date).AddSeconds(45)
while (-not (Get-NetTCPConnection -LocalPort $LocalPort -State Listen -ErrorAction SilentlyContinue)) {
    if ((Get-Date) -gt $deadline) {
        Stop-Setup "the port forward did not open within 45 seconds. Read the error in the port forward window."
    }
    Start-Sleep -Seconds 1
}

Write-Host ''
Write-Host "Port forward is open on localhost:$LocalPort." -ForegroundColor Green
Write-Host 'Point the worker at it in .env (docs/windows.md). The database requires TLS, so keep sslmode=require:'
Write-Host "    DATABASE_URL=postgres://wt_pipeline_login:<password>@localhost:$LocalPort/wt?sslmode=require"
Write-Host "The password is the secret $StackName/db_wt_pipeline_login. Percent-encode it if it has symbols."
Write-Host 'Then run: wt worker --kinds audio --concurrency 1'
