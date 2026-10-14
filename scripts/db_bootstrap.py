"""One-time database bootstrap for a new v1 environment, run by a person, never by CI.

Runs the wt-v1-admin task (which connects as the RDS master user) to:
  1. apply the migrations, which create the Spec 02 group roles
  2. create wt_owner_login, wt_web_login and wt_pipeline_login with the passwords Terraform
     generated into Secrets Manager (ECS injects them; they never pass through this script)
then runs the wt-v1-migrate task once as wt_owner_login to prove the owner login works.

Every step is idempotent, so rerunning it is safe, for example after rotating passwords.

Needs the AWS CLI with an admin profile (for example AWS_PROFILE=wheel-turning) and the
admin and migrate task definitions pointing at a pushed image (terraform apply with
migrate_image_tag set).
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys


def aws(*args: str) -> dict:
    """Runs an AWS CLI command and parses its JSON output, or {} when it prints nothing."""
    out = subprocess.run(
        ["aws", *args, "--output", "json"], check=True, capture_output=True, text=True
    )
    return json.loads(out.stdout) if out.stdout.strip() else {}


def tf_output(tf_dir: str) -> dict:
    """The environment's Terraform outputs as a flat name-to-value map."""
    out = subprocess.run(
        ["terraform", f"-chdir={tf_dir}", "output", "-json"],
        check=True,
        capture_output=True,
        text=True,
    )
    return {k: v["value"] for k, v in json.loads(out.stdout).items()}


def run_task(outputs: dict, family: str, command: list[str] | None, label: str) -> None:
    """Runs one Fargate task to completion, prints its log, and exits if it failed.

    The task runs in the public subnets with the web group, the one the database accepts.
    `command` overrides the container's command; None keeps the task definition's.
    """
    network = {
        "awsvpcConfiguration": {
            "subnets": outputs["public_subnet_ids"],
            "securityGroups": [outputs["security_group_ids"]["web"]],
            "assignPublicIp": "ENABLED",
        }
    }
    args = [
        "ecs", "run-task",
        "--cluster", outputs["cluster_name"],
        "--task-definition", family,
        "--launch-type", "FARGATE",
        "--network-configuration", json.dumps(network),
    ]  # fmt: skip
    container = family.rsplit("-", 1)[-1]
    if command:
        overrides = {"containerOverrides": [{"name": container, "command": command}]}
        args += ["--overrides", json.dumps(overrides)]
    task_arn = aws(*args)["tasks"][0]["taskArn"]
    print(f"{label}: started {task_arn.rsplit('/', 1)[-1]}", flush=True)
    subprocess.run(
        [
            "aws", "ecs", "wait", "tasks-stopped",
            "--cluster", outputs["cluster_name"], "--tasks", task_arn,
        ],
        check=True,
    )  # fmt: skip
    task = aws("ecs", "describe-tasks", "--cluster", outputs["cluster_name"], "--tasks", task_arn)
    task = task["tasks"][0]
    code = task["containers"][0].get("exitCode")
    stream = f"{container}/{container}/{task_arn.rsplit('/', 1)[-1]}"
    try:
        events = aws(
            "logs", "get-log-events", "--log-group-name", "/wt-v1/admin",
            "--log-stream-name", stream,
        )["events"]  # fmt: skip
        for event in events:
            print(f"    {event['message']}")
    except subprocess.CalledProcessError:
        print("    (no log output)")
    if code != 0:
        reason = task.get("stoppedReason", "")
        raise SystemExit(f"{label} failed: exit code {code} {reason}".strip())
    print(f"{label}: ok")


def main() -> int:
    """Migrates as master, creates the three logins, then proves the owner login can migrate."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--tf-dir", default="infra/terraform/envs/v1")
    args = parser.parse_args()
    outputs = tf_output(args.tf_dir)
    families = outputs["task_definition_families"]

    run_task(outputs, families["admin"], ["wt", "db", "migrate"], "migrate as master")
    for group in ("wt_owner", "wt_web", "wt_pipeline"):
        login = f"{group}_login"
        command = [
            "wt", "db", "create-login", "--role", group, "--login", login,
            "--password-env", f"WT_LOGIN_PASSWORD_{login.upper()}",
        ]  # fmt: skip
        run_task(outputs, families["admin"], command, f"create {login}")
    run_task(outputs, families["migrate"], None, "migrate as wt_owner_login")
    return 0


if __name__ == "__main__":
    sys.exit(main())
