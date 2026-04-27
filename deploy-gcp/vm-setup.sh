#!/usr/bin/env bash
# VM bootstrap for T3Claw on GCP Compute Engine (Debian 12).
#
# Copy this directory and docker-compose.yml to the VM first, then run:
#   sudo bash /tmp/deploy/vm-setup.sh
#
# The script expects:
#   /tmp/deploy/              — contents of deploy-gcp/
#   /tmp/docker-compose.yml   — repo-root docker-compose.yml

set -euo pipefail

if [ "$(id -u)" -ne 0 ]; then
  echo "ERROR: run as root: sudo bash vm-setup.sh"
  exit 1
fi

REGION="${REGION:-us-central1}"
PROJECT="${PROJECT:-gen-lang-client-0263867259}"
REPO="t3claw"
IMAGE_PREFIX="${REGION}-docker.pkg.dev/${PROJECT}/${REPO}"

# ── Docker (official repo — Debian 12 default repos lack docker-compose-plugin)
echo "==> Installing Docker"
apt-get update -qq
apt-get install -y --no-install-recommends ca-certificates curl gnupg
install -m 0755 -d /etc/apt/keyrings
curl -fsSL https://download.docker.com/linux/debian/gpg \
  -o /etc/apt/keyrings/docker.asc
chmod a+r /etc/apt/keyrings/docker.asc
ARCH=$(dpkg --print-architecture)
CODENAME=$(. /etc/os-release && echo "$VERSION_CODENAME")
echo "deb [arch=${ARCH} signed-by=/etc/apt/keyrings/docker.asc] https://download.docker.com/linux/debian ${CODENAME} stable" \
  > /etc/apt/sources.list.d/docker.list
apt-get update -qq
apt-get install -y --no-install-recommends \
  docker-ce docker-ce-cli containerd.io docker-buildx-plugin docker-compose-plugin
systemctl enable docker
systemctl start docker

# ── Artifact Registry auth (uses the attached VM service account) ─────────────
echo "==> Configuring Artifact Registry auth"
# Install gcloud CLI if not present (Debian 12 base images may not include it)
if ! command -v gcloud &>/dev/null; then
  apt-get install -y --no-install-recommends apt-transport-https ca-certificates gnupg curl
  curl -fsSL https://packages.cloud.google.com/apt/doc/apt-key.gpg \
    | gpg --dearmor -o /usr/share/keyrings/cloud.google.gpg
  echo "deb [signed-by=/usr/share/keyrings/cloud.google.gpg] https://packages.cloud.google.com/apt cloud-sdk main" \
    > /etc/apt/sources.list.d/google-cloud-sdk.list
  apt-get update -qq
  apt-get install -y google-cloud-cli
fi

gcloud auth configure-docker "${REGION}-docker.pkg.dev" --quiet

# Pre-pull images so the first start is fast (the service does this too, but
# doing it here surfaces auth problems before systemd gets involved).
echo "==> Pre-pulling images"
docker pull "${IMAGE_PREFIX}/agent:latest"
docker pull "${IMAGE_PREFIX}/t3n-mcp-sidecar:latest"

# ── App directory ─────────────────────────────────────────────────────────────
echo "==> Setting up /opt/t3claw"
mkdir -p /opt/t3claw
chmod 700 /opt/t3claw

cp /tmp/docker-compose.yml /opt/t3claw/docker-compose.yml

# Rewrite image references so compose uses the Artifact Registry images instead
# of building from source (the VM has no source tree).
sed -i \
  "s|build:.*||g;
   /context:/d;
   /dockerfile:/d;
   /target:/d;
   s|image: t3claw.*|image: ${IMAGE_PREFIX}/agent:latest|g" \
  /opt/t3claw/docker-compose.yml

# ── Environment file ──────────────────────────────────────────────────────────
if [ ! -f /opt/t3claw/.env ]; then
  echo ""
  echo "WARNING: /opt/t3claw/.env does not exist."
  echo "Create it with your secrets before starting the service."
  echo "See deploy-gcp/env.example for the required variables."
  echo ""
  echo "Once .env is in place, run:"
  echo "  systemctl enable t3claw && systemctl start t3claw"
else
  chmod 600 /opt/t3claw/.env
fi

# ── Systemd service ───────────────────────────────────────────────────────────
echo "==> Installing t3claw.service"
cp /tmp/deploy/t3claw.service /etc/systemd/system/t3claw.service
systemctl daemon-reload

if [ -f /opt/t3claw/.env ]; then
  echo "==> Starting T3Claw"
  systemctl enable t3claw
  systemctl start t3claw
fi

echo ""
echo "==> Bootstrap complete"
echo ""
echo "    Verify with:"
echo "      systemctl status t3claw"
echo "      docker logs t3-claw-t3claw-1"
