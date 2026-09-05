#!/usr/bin/env bash
# Terminate the measurement instance for a project.
#
# Usage:
#   ./terminate.sh --project NAME [--region REGION] [--yes]
#
# Options:
#   --project NAME   Required. Reads ~/.bench-env/NAME.state for the instance.
#   --region REGION  Default from the state file, then the AWS CLI config.
#   --yes            Skip the confirmation prompt.
#   -h, --help       Show this help.
#
# Only terminates an instance that is recorded in the project's state file AND
# carries the managed-by=bench-env tag. Instance ids get reused, so a stale
# state file would otherwise terminate someone else's work.

set -euo pipefail

usage() { awk 'NR<=16 && /^#( |$)/ { sub(/^# ?/, ""); print }' "$0"; }

PROJECT=""
REGION=""
ASSUME_YES=0

while [[ $# -gt 0 ]]; do
  case "$1" in
    --project) PROJECT="$2"; shift 2;;
    --region)  REGION="$2"; shift 2;;
    --yes)     ASSUME_YES=1; shift;;
    -h|--help) usage; exit 0;;
    *) echo "error: unknown argument '$1'" >&2; usage >&2; exit 2;;
  esac
done

[[ -z "$PROJECT" ]] && { echo "error: --project is required" >&2; exit 2; }

STATE="$HOME/.bench-env/$PROJECT.state"
[[ -f "$STATE" ]] || { echo "error: no state file at $STATE" >&2; exit 2; }

INSTANCE_ID=$(awk -F'\t' '$1=="instance_id"{print $2}' "$STATE")
[[ -z "$INSTANCE_ID" ]] && { echo "error: no instance_id in $STATE" >&2; exit 2; }
[[ -z "$REGION" ]] && REGION=$(awk -F'\t' '$1=="region"{print $2}' "$STATE")

AWS=(aws)
[[ -n "$REGION" && "$REGION" != "unknown" ]] && AWS+=(--region "$REGION")

read -r STATE_NAME TAG_MANAGED TAG_PROJECT < <(
  "${AWS[@]}" ec2 describe-instances --instance-ids "$INSTANCE_ID" \
    --query 'Reservations[0].Instances[0].[State.Name,Tags[?Key==`managed-by`]|[0].Value,Tags[?Key==`project`]|[0].Value]' \
    --output text 2>/dev/null || echo "missing - -"
)

if [[ "$STATE_NAME" == "missing" ]]; then
  echo "$INSTANCE_ID no longer exists; removing stale state file."
  rm -f "$STATE"
  exit 0
fi

if [[ "$STATE_NAME" == "terminated" || "$STATE_NAME" == "shutting-down" ]]; then
  echo "$INSTANCE_ID is already $STATE_NAME; removing state file."
  rm -f "$STATE"
  exit 0
fi

# Refuse anything this tool did not create.
if [[ "$TAG_MANAGED" != "bench-env" || "$TAG_PROJECT" != "$PROJECT" ]]; then
  cat >&2 <<EOF
error: $INSTANCE_ID is not a bench-env instance for '$PROJECT'.
         managed-by : ${TAG_MANAGED:-<none>}  (expected bench-env)
         project    : ${TAG_PROJECT:-<none>}  (expected $PROJECT)
       Refusing to terminate. The state file may be stale; delete it by hand
       after checking what that instance actually is:
         $STATE
EOF
  exit 5
fi

if [[ $ASSUME_YES -eq 0 ]]; then
  echo "about to terminate $INSTANCE_ID ($STATE_NAME, project=$PROJECT)."
  read -r -p "proceed? [y/N] " reply
  [[ "$reply" =~ ^[Yy]$ ]] || { echo "aborted."; exit 0; }
fi

"${AWS[@]}" ec2 terminate-instances --instance-ids "$INSTANCE_ID" \
  --query 'TerminatingInstances[0].CurrentState.Name' --output text
rm -f "$STATE"
echo "terminated $INSTANCE_ID; removed $STATE"
