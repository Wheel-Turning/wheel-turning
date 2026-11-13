"""Load the Well-Architected ledger into the AWS Well-Architected Tool.

docs/well-architected/bp-ledger.csv holds one row per framework best practice with its status.
This script maps each row onto the Tool's answer for its question: Implemented best practices
become selected choices, Not Applicable ones are marked not applicable, and the rest stay
unselected with their gap in the question's notes. The Tool's choice ids are the slugs of the
best-practice pages, which the ledger keeps in `choice_id`.

Read-only unless told otherwise. With the owner's profile (`aws sso login --profile
wheel-turning`), from the repository root:

    python scripts/ops/wa_tool_import.py --create                 # make the workload, print its id
    python scripts/ops/wa_tool_import.py --workload-id <id>       # dry run: print every update
    python scripts/ops/wa_tool_import.py --workload-id <id> --apply

`--create` and `--apply` write to the Tool; nothing else does. Rerunning `--apply` sets the same
answers again. It calls the AWS CLI, so it needs no Python packages.
"""

from __future__ import annotations

import argparse
import csv
import json
import os
import subprocess
import sys
from collections import defaultdict
from pathlib import Path

LENS = "wellarchitected"
LEDGER = Path(__file__).resolve().parents[2] / "docs" / "well-architected" / "bp-ledger.csv"
# The Tool's limits: question notes 2084 characters, choice notes 250.
QUESTION_NOTES_MAX = 2084
CHOICE_NOTES_MAX = 250


def aws(*args: str) -> dict:
    """Run one AWS CLI call and return its JSON output."""
    env = {**os.environ, "AWS_PAGER": "", "MSYS_NO_PATHCONV": "1"}
    result = subprocess.run(
        ["aws", *args, "--output", "json"],
        capture_output=True,
        text=True,
        env=env,
        check=False,
    )
    if result.returncode != 0:
        sys.exit(f"aws {' '.join(args[:2])} failed: {result.stderr.strip()}")
    return json.loads(result.stdout or "{}")


def read_ledger(path: Path) -> dict[str, list[dict[str, str]]]:
    """Ledger rows grouped by framework question id (OPS01, SEC02, ...)."""
    by_question: dict[str, list[dict[str, str]]] = defaultdict(list)
    with path.open(encoding="utf-8", newline="") as f:
        for row in csv.DictReader(f):
            by_question[row["question_id"]].append(row)
    return by_question


def tool_answers(workload_id: str) -> list[dict]:
    """Every answer of the framework lens in the workload, with its choices."""
    # list-answers returns a page at a time and the CLI does not page it on its own.
    summaries: list[dict] = []
    token: list[str] = []
    while True:
        page = aws(
            "wellarchitected",
            "list-answers",
            "--workload-id",
            workload_id,
            "--lens-alias",
            LENS,
            "--max-results",
            "50",
            *token,
        )
        summaries += page["AnswerSummaries"]
        if not page.get("NextToken"):
            break
        token = ["--next-token", page["NextToken"]]
    return [
        aws(
            "wellarchitected",
            "get-answer",
            "--workload-id",
            workload_id,
            "--lens-alias",
            LENS,
            "--question-id",
            s["QuestionId"],
        )["Answer"]
        for s in summaries
    ]


def plan(answer: dict, rows: list[dict[str, str]]) -> tuple[dict, list[str]]:
    """The update-answer arguments for one question, and ledger rows with no matching choice."""
    choice_ids = {c["ChoiceId"] for c in answer["Choices"]}
    by_choice = {r["choice_id"]: r for r in rows}
    unmatched = [r["bp_id"] for r in rows if r["choice_id"] not in choice_ids]

    selected = [
        cid for cid, r in by_choice.items() if cid in choice_ids and r["status"] == "Implemented"
    ]
    # A question with nothing selected counts as unanswered; its "None of these" choice says so.
    if not selected:
        selected = [cid for cid in choice_ids if cid.endswith("_no")][:1]
    updates = {}
    gaps = []
    for cid, r in by_choice.items():
        if cid not in choice_ids:
            continue
        if r["status"] == "Not Applicable":
            updates[cid] = {
                "Status": "NOT_APPLICABLE",
                "Reason": "OUT_OF_SCOPE",
                "Notes": r["evidence"][:CHOICE_NOTES_MAX],
            }
        elif r["status"] != "Implemented":
            risk = f", {r['risk']}" if r["risk"] else ""
            gaps.append(f"{r['bp_id']} ({r['status']}{risk}): {r['recommendation']}")

    applicable = any(r["status"] != "Not Applicable" for r in rows)
    notes = "From docs/well-architected/bp-ledger.csv.\n" + "\n".join(gaps)
    args = {
        "QuestionId": answer["QuestionId"],
        "SelectedChoices": selected,
        "ChoiceUpdates": updates,
        "Notes": notes[:QUESTION_NOTES_MAX],
        "IsApplicable": applicable,
    }
    return args, unmatched


def question_key(answer: dict, ledger: dict[str, list[dict[str, str]]]) -> str | None:
    """The ledger question whose choices this Tool question holds."""
    choice_ids = {c["ChoiceId"] for c in answer["Choices"]}
    for qid, rows in ledger.items():
        if any(r["choice_id"] in choice_ids for r in rows):
            return qid
    return None


def create(args: argparse.Namespace) -> None:
    """Create the workload on the framework lens and print its id."""
    out = aws(
        "wellarchitected",
        "create-workload",
        "--workload-name",
        args.name,
        "--description",
        "Private, text-only retrieval demo over Zen talks (Wheel Turning v1 beta).",
        "--environment",
        "PREPRODUCTION",
        "--aws-regions",
        args.region,
        "--review-owner",
        args.owner,
        "--lenses",
        LENS,
        "--tags",
        "project=wheel-turning,env=v1",
    )
    print(out["WorkloadId"])


def load(args: argparse.Namespace) -> None:
    """Plan, and with --apply write, one answer per framework question."""
    ledger = read_ledger(args.ledger)
    answers = tool_answers(args.workload_id)
    seen: set[str] = set()
    unmatched_all: list[str] = []
    for answer in answers:
        qid = question_key(answer, ledger)
        if qid is None:
            print(f"skip {answer['QuestionId']}: no ledger row matches its choices")
            continue
        seen.add(qid)
        update, unmatched = plan(answer, ledger[qid])
        unmatched_all += unmatched
        print(
            f"{qid} -> {update['QuestionId']}: {len(update['SelectedChoices'])} selected, "
            f"{len(update['ChoiceUpdates'])} not applicable, applicable={update['IsApplicable']}"
        )
        if args.apply:
            # One JSON document: the CLI cannot pass an empty list of selected choices.
            payload = {"WorkloadId": args.workload_id, "LensAlias": LENS, **update}
            aws("wellarchitected", "update-answer", "--cli-input-json", json.dumps(payload))
    missing = sorted(set(ledger) - seen)
    if missing:
        print(f"ledger questions with no Tool question: {', '.join(missing)}")
    if unmatched_all:
        print(f"ledger rows with no Tool choice: {', '.join(unmatched_all)}")
    if not args.apply:
        print("dry run: nothing written. Rerun with --apply to update the Tool.")


def main() -> None:
    """Parse arguments and run the chosen action."""
    p = argparse.ArgumentParser(
        description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter
    )
    p.add_argument("--ledger", type=Path, default=LEDGER)
    p.add_argument("--create", action="store_true", help="create the workload and print its id")
    p.add_argument("--workload-id", help="load the ledger into this workload")
    p.add_argument("--apply", action="store_true", help="write the answers (default: dry run)")
    p.add_argument("--name", default="wheel-turning-v1")
    p.add_argument("--region", default="us-east-2")
    p.add_argument("--owner", default="<OWNER_NAME>")
    args = p.parse_args()
    if args.create:
        create(args)
    elif args.workload_id:
        load(args)
    else:
        p.error("give --create or --workload-id")


if __name__ == "__main__":
    main()
