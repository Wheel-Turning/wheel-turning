"""Spec 01 M1: prove the database accepts connections only from the web and batch groups.

Checks the live AWS configuration, not the Terraform source:
- the RDS instance is not publicly accessible and sits only in the wt-v1-db security group
- that group's inbound rules are exactly TCP 5432 from wt-v1-web and from wt-v1-batch, with no
  CIDR, prefix-list or other group sources, and it has no outbound rules
- the instance's subnets have no route to an internet gateway

Exits nonzero on any finding. Needs the AWS CLI with credentials for the account.
"""

from __future__ import annotations

import argparse
import json
import subprocess
import sys


def aws(*args: str) -> dict:
    out = subprocess.run(
        ["aws", *args, "--output", "json"], check=True, capture_output=True, text=True
    )
    return json.loads(out.stdout)


def group_id(name: str) -> str:
    groups = aws("ec2", "describe-security-groups", "--filters", f"Name=group-name,Values={name}")
    found = groups["SecurityGroups"]
    if len(found) != 1:
        raise SystemExit(f"expected one security group named {name}, found {len(found)}")
    return found[0]["GroupId"]


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    parser.add_argument("--name", default="wt-v1", help="stack name prefix")
    args = parser.parse_args()
    n = args.name
    failures: list[str] = []

    db_sg, web_sg, batch_sg = (group_id(f"{n}-{role}") for role in ("db", "web", "batch"))

    db = aws("rds", "describe-db-instances", "--db-instance-identifier", n)["DBInstances"][0]
    if db["PubliclyAccessible"]:
        failures.append("database is publicly accessible")
    attached = sorted(g["VpcSecurityGroupId"] for g in db["VpcSecurityGroups"])
    if attached != [db_sg]:
        failures.append(f"database security groups are {attached}, expected only {db_sg}")

    rules = aws(
        "ec2", "describe-security-group-rules", "--filters", f"Name=group-id,Values={db_sg}"
    )["SecurityGroupRules"]
    sources = set()
    for rule in rules:
        if rule["IsEgress"]:
            failures.append(f"database group has an outbound rule: {rule['SecurityGroupRuleId']}")
            continue
        ref = rule.get("ReferencedGroupInfo", {}).get("GroupId")
        ok_port = rule["IpProtocol"] == "tcp" and rule["FromPort"] == rule["ToPort"] == 5432
        if not ok_port or ref is None or rule.get("CidrIpv4") or rule.get("CidrIpv6"):
            failures.append(f"unexpected inbound rule: {json.dumps(rule)}")
            continue
        sources.add(ref)
    if sources != {web_sg, batch_sg}:
        failures.append(
            f"inbound 5432 sources are {sorted(sources)}, "
            f"expected web {web_sg} and batch {batch_sg}"
        )

    subnet_ids = [s["SubnetIdentifier"] for s in db["DBSubnetGroup"]["Subnets"]]
    tables = aws(
        "ec2",
        "describe-route-tables",
        "--filters",
        f"Name=association.subnet-id,Values={','.join(subnet_ids)}",
    )["RouteTables"]
    for table in tables:
        for route in table["Routes"]:
            if route.get("GatewayId", "").startswith("igw-"):
                failures.append(
                    f"database subnet route table {table['RouteTableId']} reaches an IGW"
                )

    if failures:
        for failure in failures:
            print(f"FAIL: {failure}", file=sys.stderr)
        return 1
    print(
        f"ok: {n} database is private and accepts 5432 only from {n}-web ({web_sg}) "
        f"and {n}-batch ({batch_sg})"
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
