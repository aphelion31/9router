#!/bin/sh
# Redeploy 9router (develop) on vps-circleit behind Traefik. Same flow as pulsar:
# start new build on a temp name, check /api/health, then swap. Old binary kept as ninerouter.prev.
# Usage (on the VPS): sh ~/9router/deploy-circleit.sh   (expects ~/9router/ninerouter.new + .env)
set -eu
cd ~/9router
H=${NINEROUTER_HOST:-9routertest.circleit.dev}
PORT=20127
run() { # $1 name, $2 binary file, extra args: traefik labels for the routed container
  name=$1; bin=$2; shift 2
  docker run -d --name "$name" --restart unless-stopped --cpus 1 --memory 384m --env-file .env \
    -e HOST=0.0.0.0 -e PORT=$PORT -e DATA_DIR=/data -e TRUST_PROXY=true \
    -v "$HOME/9router/$bin:/app/ninerouter:ro" -v "$HOME/9router/data:/data" \
    -v /etc/ssl/certs/ca-certificates.crt:/etc/ssl/certs/ca-certificates.crt:ro \
    --network traefik-network --log-opt max-size=10m --log-opt max-file=3 "$@" alpine:latest /app/ninerouter serve >/dev/null
}
health() { docker run --rm --network traefik-network curlimages/curl:latest -sf -o /dev/null "http://$1:$PORT/api/health"; }

mkdir -p data
chmod +x ninerouter.new
docker rm -f ninerouter-next >/dev/null 2>&1 || true
run ninerouter-next ninerouter.new
ok=0; for i in $(seq 1 20); do if health ninerouter-next; then ok=1; break; fi; sleep 1; done
if [ $ok != 1 ]; then echo "new build unhealthy, keeping current"; docker logs ninerouter-next 2>&1 | tail -5; docker rm -f ninerouter-next >/dev/null; exit 1; fi
docker rm -f ninerouter-next >/dev/null

[ -f ninerouter ] && cp ninerouter ninerouter.prev
mv ninerouter.new ninerouter
docker rm -f ninerouter >/dev/null 2>&1 || true
run ninerouter ninerouter \
  -l traefik.enable=true -l traefik.docker.network=traefik-network \
  -l "traefik.http.routers.ninerouter-router.rule=Host(\`$H\`)" -l traefik.http.routers.ninerouter-router.entryPoints=websecure \
  -l traefik.http.routers.ninerouter-router.tls=true -l traefik.http.routers.ninerouter-router.tls.certresolver=leResolver \
  -l traefik.http.routers.ninerouter-router.service=ninerouter-service -l traefik.http.services.ninerouter-service.loadbalancer.server.port=$PORT
for i in $(seq 1 20); do health ninerouter && break; sleep 1; done
for i in $(seq 1 30); do [ "$(curl -s -o /dev/null -w '%{http_code}' "https://$H/api/health")" = 200 ] && break; sleep 1; done
echo "public /api/health: $(curl -s -o /dev/null -w '%{http_code}' "https://$H/api/health")"
docker ps --filter name=^ninerouter$ --format "{{.Names}} {{.Status}}"
