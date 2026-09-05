#!/usr/bin/env bash
# Launch one exclusive measurement instance.
#
# Usage:
#   ./launch.sh --project NAME [--instance-type TYPE] [--ttl MINUTES]
#               [--region REGION] [--key-name KEY]
#
# Options:
#   --project NAME      Required. Tags the instance and names the state file.
#   --instance-type T   Default m5d.large. See README for why not t3.
#   --ttl MINUTES       Hard shutdown after this long. Default 180.
#   --region REGION     Default from the AWS CLI configuration.
#   --key-name KEY      EC2 key pair for ssh. Default from BENCH_ENV_KEY.
#   -h, --help          Show this help.
#
# One run gets one instance, and the instance is the lock: nothing else is
# measuring on it. See README for why a shared instance cannot produce valid
# measurements.

set -euo pipefail

usage() { awk 'NR<=20 && /^#( |$)/ { sub(/^# ?/, ""); print }' "$0"; }

PROJECT=""
INSTANCE_TYPE="m5d.large"
TTL=180
REGION=""
KEY_NAME="${BENCH_ENV_KEY:-}"

while [[ $# -gt 0 ]]; do
  case "$1" in
    --project)       PROJECT="$2"; shift 2;;
    --instance-type) INSTANCE_TYPE="$2"; shift 2;;
    --ttl)           TTL="$2"; shift 2;;
    --region)        REGION="$2"; shift 2;;
    --key-name)      KEY_NAME="$2"; shift 2;;
    -h|--help)       usage; exit 0;;
    *) echo "error: unknown argument '$1'" >&2; usage >&2; exit 2;;
  esac
done

[[ -z "$PROJECT" ]] && { echo "error: --project is required" >&2; exit 2; }
[[ -z "$KEY_NAME" ]] && {
  echo "error: no EC2 key pair. Pass --key-name or set BENCH_ENV_KEY." >&2; exit 2; }

case "$INSTANCE_TYPE" in
  t2.*|t3.*|t3a.*|t4g.*)
    cat >&2 <<EOF
error: '$INSTANCE_TYPE' is a burstable type. Its CPU and EBS bandwidth are
       credit-based, so the same measurement changes as credits drain -- this
       has already corrupted one set of results. Use m5d.large (default) or
       another fixed-performance type.
       Edit this check only if you are deliberately measuring burst behaviour.
EOF
    exit 2;;
esac

SCRIPT_DIR=$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)
PROVISION="$SCRIPT_DIR/../provision/setup.sh"
[[ -r "$PROVISION" ]] || { echo "error: provision script not found at $PROVISION" >&2; exit 2; }

AWS=(aws)
[[ -n "$REGION" ]] && AWS+=(--region "$REGION")

# Fail early and clearly rather than deep inside a run-instances error.
"${AWS[@]}" sts get-caller-identity >/dev/null 2>&1 || {
  echo "error: AWS credentials are not valid. Refresh them and retry." >&2; exit 3; }

STATE_DIR="$HOME/.bench-env"
STATE="$STATE_DIR/$PROJECT.state"
mkdir -p "$STATE_DIR"

if [[ -f "$STATE" ]]; then
  existing=$(awk -F'\t' '$1=="instance_id"{print $2}' "$STATE")
  state=$("${AWS[@]}" ec2 describe-instances --instance-ids "$existing" \
            --query 'Reservations[].Instances[].State.Name' --output text 2>/dev/null || true)
  if [[ "$state" == "running" || "$state" == "pending" ]]; then
    echo "error: '$PROJECT' already has instance $existing ($state)." >&2
    echo "       Use it, or run ./terminate.sh --project $PROJECT first." >&2
    exit 4
  fi
fi

# Amazon Linux 2023, current x86_64 AMI, resolved through SSM so the ID is not
# pinned to one that will later be deregistered.
AMI=$("${AWS[@]}" ssm get-parameters \
        --names /aws/service/ami-amazon-linux-latest/al2023-ami-kernel-default-x86_64 \
        --query 'Parameters[0].Value' --output text)
[[ -z "$AMI" || "$AMI" == "None" ]] && { echo "error: could not resolve an AMI" >&2; exit 3; }

# user-data: a hard TTL, then the provisioning script. The TTL is what keeps a
# forgotten instance from billing indefinitely.
USER_DATA=$(cat <<EOF
#!/bin/bash
shutdown -h +$TTL &
$(cat "$PROVISION")
EOF
)

echo "launching $INSTANCE_TYPE for '$PROJECT' (AMI $AMI, TTL ${TTL}m)..."
INSTANCE_ID=$("${AWS[@]}" ec2 run-instances \
  --image-id "$AMI" \
  --instance-type "$INSTANCE_TYPE" \
  --key-name "$KEY_NAME" \
  --instance-initiated-shutdown-behavior terminate \
  --metadata-options "HttpTokens=required,HttpEndpoint=enabled" \
  --user-data "$USER_DATA" \
  --tag-specifications \
    "ResourceType=instance,Tags=[{Key=Name,Value=bench-env-$PROJECT},{Key=project,Value=$PROJECT},{Key=managed-by,Value=bench-env}]" \
  --query 'Instances[0].InstanceId' --output text)

echo "instance: $INSTANCE_ID -- waiting for it to run..."
"${AWS[@]}" ec2 wait instance-running --instance-ids "$INSTANCE_ID"

PUBLIC_IP=$("${AWS[@]}" ec2 describe-instances --instance-ids "$INSTANCE_ID" \
  --query 'Reservations[0].Instances[0].PublicIpAddress' --output text)

REGION_USED="${REGION:-$("${AWS[@]}" configure get region 2>/dev/null || echo unknown)}"
{
  printf 'instance_id\t%s\n'   "$INSTANCE_ID"
  printf 'public_ip\t%s\n'     "$PUBLIC_IP"
  printf 'instance_type\t%s\n' "$INSTANCE_TYPE"
  printf 'region\t%s\n'        "$REGION_USED"
  printf 'launched_at\t%s\n'   "$(date -u '+%Y-%m-%dT%H:%M:%SZ')"
  printf 'ttl_minutes\t%s\n'   "$TTL"
} > "$STATE"

cat <<EOF

instance is running, but provisioning is still in progress.
"running" and "ready to measure on" are not the same thing -- wait for the
marker before measuring:

  ssh ec2-user@$PUBLIC_IP 'while [ ! -f /var/lib/bench-env/ready ]; do sleep 5; done; echo ready'

then:

  ssh ec2-user@$PUBLIC_IP
  # /mnt/xfs /mnt/ext4 /mnt/btrfs are prepared
  # source protocol/lib.sh for the measurement rules

terminate when done (or let the ${TTL}m TTL do it):

  ./terminate.sh --project $PROJECT

state written to $STATE
EOF
