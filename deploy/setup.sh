#!/usr/bin/env bash
# VM bootstrap script for T3Claw on GCP Compute Engine.
#
# Run on a fresh Debian 12 VM after SSH:
#   sudo bash setup.sh
#
# Prerequisites:
#   - VM has the t3claw-vm service account attached
#   - Cloud SQL Auth Proxy accessible via IAM
#   - Artifact Registry image pushed

set -euo pipefail

# Must run as root
if [ "$(id -u)" -ne 0 ]; then
  echo "ERROR: This script must be run as root (sudo bash setup.sh)"
  exit 1
fi

echo "==> Installing Docker"
apt-get update
apt-get install -y docker.io
systemctl enable docker
systemctl start docker

echo "==> Installing Cloud SQL Auth Proxy"
CLOUD_SQL_PROXY_VERSION="v2.14.3"
CLOUD_SQL_PROXY_SHA256="75e7cc1f158ab6f97b7810e9d8419c55735cff40bc56d4f19673adfdf2406a59"
curl -fsSL -o /usr/local/bin/cloud-sql-proxy \
  "https://storage.googleapis.com/cloud-sql-connectors/cloud-sql-proxy/${CLOUD_SQL_PROXY_VERSION}/cloud-sql-proxy.linux.amd64"
echo "${CLOUD_SQL_PROXY_SHA256}  /usr/local/bin/cloud-sql-proxy" | sha256sum -c - || {
  echo "ERROR: Cloud SQL Auth Proxy checksum verification failed -- aborting"
  rm -f /usr/local/bin/cloud-sql-proxy
  exit 1
}
chmod +x /usr/local/bin/cloud-sql-proxy

echo "==> Installing systemd services"
cp /tmp/deploy/cloud-sql-proxy.service /etc/systemd/system/
cp /tmp/deploy/t3claw.service /etc/systemd/system/
systemctl daemon-reload

echo "==> Starting Cloud SQL Auth Proxy"
systemctl enable cloud-sql-proxy
systemctl start cloud-sql-proxy

echo "==> Configuring Docker registry auth"
# The VM service account provides Artifact Registry access
gcloud auth configure-docker us-central1-docker.pkg.dev --quiet

echo "==> Creating config directory"
# Owned by root, readable only by root. Docker reads --env-file as root
# before dropping to uid 1000 (t3claw) inside the container.
mkdir -p /opt/t3claw
chmod 700 /opt/t3claw

if [ ! -f /opt/t3claw/.env ]; then
  echo "WARNING: /opt/t3claw/.env does not exist."
  echo "Create it with your configuration before starting T3Claw."
  echo "See deploy/env.example for the required variables."
  echo ""
  echo "Then run: systemctl enable t3claw && systemctl start t3claw"
else
  chmod 600 /opt/t3claw/.env
  echo "==> Starting T3Claw"
  systemctl enable t3claw
  systemctl start t3claw
fi

echo "==> Setup complete"
echo ""
echo "Verify with:"
echo "  systemctl status cloud-sql-proxy"
echo "  systemctl status t3claw"
echo "  docker logs t3claw"
