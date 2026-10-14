"""Spec 01 M1: verify the scratch bucket's one-day expiry rule.

Reads the scratch bucket's lifecycle configuration and asserts an enabled rule that covers the
whole bucket, expires objects after one day, and aborts incomplete multipart uploads after one
day. Exits nonzero if any of that is missing.

The M1 "done when" also wants a test object to expire. That takes about two days, so it is a
separate two-step check:

    python scripts/check_scratch_lifecycle.py --put-probe     # writes a small text object
    python scripts/check_scratch_lifecycle.py --check-probe   # 48+ hours later: must be gone

Needs the AWS CLI with credentials for the account (for example AWS_PROFILE=wheel-turning).
The probe is plain text: audio must never be written anywhere but the scratch directory.
"""

from __future__ import annotations

import argparse
import datetime as dt
import json
import subprocess
import sys
import tempfile
from pathlib import Path

PROBE_KEY = "lifecycle-probe/probe.txt"


def aws(*args: str) -> dict:
    """Runs an AWS CLI command and parses its JSON output, or {} when it prints nothing."""
    out = subprocess.run(
        ["aws", *args, "--output", "json"], check=True, capture_output=True, text=True
    )
    return json.loads(out.stdout) if out.stdout.strip() else {}


def default_bucket() -> str:
    """The bucket Terraform names after the account: wt-v1-scratch-<account id>."""
    account = aws("sts", "get-caller-identity")["Account"]
    return f"wt-v1-scratch-{account}"


def covers_whole_bucket(rule: dict) -> bool:
    """Whether a lifecycle rule applies to every key, under either the legacy or the Filter form."""
    if rule.get("Prefix"):  # legacy top-level prefix
        return False
    filt = rule.get("Filter")
    if filt is None or filt == {}:
        return True
    return filt.get("Prefix", None) == "" and len(filt) == 1


def check_rule(bucket: str) -> list[str]:
    """The findings for the lifecycle rules: none when one enabled rule sets both expiries."""
    config = aws("s3api", "get-bucket-lifecycle-configuration", "--bucket", bucket)
    rules = config.get("Rules", [])
    for rule in rules:
        if rule.get("Status") != "Enabled" or not covers_whole_bucket(rule):
            continue
        expiry = rule.get("Expiration", {}).get("Days")
        abort = rule.get("AbortIncompleteMultipartUpload", {}).get("DaysAfterInitiation")
        if expiry == 1 and abort == 1:
            return []
    return [
        "no enabled whole-bucket rule with Expiration.Days == 1 and "
        "AbortIncompleteMultipartUpload.DaysAfterInitiation == 1",
        f"rules found: {json.dumps(rules)}",
    ]


def put_probe(bucket: str) -> None:
    """Writes the plain-text probe object that `--check-probe` later expects to be gone."""
    stamp = dt.datetime.now(dt.UTC).isoformat()
    with tempfile.TemporaryDirectory() as tmp:
        path = Path(tmp) / "probe.txt"
        path.write_text(f"lifecycle probe written {stamp}\n", encoding="utf-8")
        aws("s3api", "put-object", "--bucket", bucket, "--key", PROBE_KEY, "--body", str(path))
    print(f"probe written to s3://{bucket}/{PROBE_KEY} at {stamp}")
    print("run --check-probe after 48 hours; S3 applies expiry asynchronously")


def check_probe(bucket: str) -> list[str]:
    """The findings for the probe: none once S3 has expired it (head-object returns 404)."""
    result = subprocess.run(
        ["aws", "s3api", "head-object", "--bucket", bucket, "--key", PROBE_KEY],
        capture_output=True,
        text=True,
    )
    if result.returncode == 0:
        head = json.loads(result.stdout)
        return [f"probe still present, last modified {head.get('LastModified')}"]
    if "404" in result.stderr or "Not Found" in result.stderr:
        return []
    return [f"could not check the probe: {result.stderr.strip()}"]


def main() -> int:
    """Checks the rule by default, or writes or checks the probe."""
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--bucket", help="defaults to wt-v1-scratch-<account id>")
    mode = parser.add_mutually_exclusive_group()
    mode.add_argument("--put-probe", action="store_true", help="write the expiry probe object")
    mode.add_argument("--check-probe", action="store_true", help="assert the probe has expired")
    args = parser.parse_args()
    bucket = args.bucket or default_bucket()

    if args.put_probe:
        put_probe(bucket)
        return 0

    failures = check_probe(bucket) if args.check_probe else check_rule(bucket)
    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    what = "probe object has expired" if args.check_probe else "one-day expiry rule present"
    print(f"ok: s3://{bucket}: {what}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
