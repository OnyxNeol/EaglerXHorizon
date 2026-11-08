# Self-hosting EaglerXHorizon on Oracle Cloud Always Free Tier
# Tested shape: Ampere A1 (aarch64), Ubuntu 24.04, Java 21, nginx in front.

This runbook stands up the browser-facing proxy that the EaglerXHorizon launcher
dials. The client itself is static files and the Rust/WASM engine; nothing here
needs a Node runtime.

---

## 0. What you get for free

| Resource | Always Free allowance | What to use |
| --- | --- | --- |
| Ampere A1 (ARM) | 4 OCPU / 24 GB total | one 4/24 instance, or two 2/12 |
| Block storage | 200 GB | 50 GB boot is plenty |
| Outbound transfer | 10 TB/month | far beyond a small server |
| Public IPv4 | 1 reserved | reserve one, never unassign it |

A single Ampere A1 core handles a few hundred idle browser sessions; the backend
JVM and the proxy both fit comfortably in 24 GB.

---

## 1. Create the instance

1. Console → Compute → Instances → **Create instance**.
2. Image: **Ubuntu 24.04 (aarch64)**, shape: **VM.Standard.A1.Flex**, 4 OCPU / 24 GB.
3. Networking: create or pick a VCN, assign a **public IPv4**, and let it create the
   security list so you have something to edit in step 2.
4. Add your SSH key, create, and note the public IP.

---

## 2. Open the ports (two places, both matter)

Oracle blocks traffic at the VCN security list **and** the instance's own firewall.
Missing either one produces a silent timeout.

**a) VCN security list / NSG** (Console → Networking → VCN → Security Lists):

| Direction | Protocol | Port | Purpose |
| --- | --- | --- | --- |
| Ingress | TCP | 22 | SSH |
| Ingress | TCP | 80 | ACME HTTP-01 + redirect to 443 |
| Ingress | TCP | 443 | wss:// proxy and dashboard |
| Ingress | TCP | 8081 | optional: plain ws for local testing only |
| Egress | All | all | default |

**b) Host firewall.** Oracle's Ubuntu images ship iptables rules that survive reboot:

```bash
sudo iptables -I INPUT 6 -m state --state NEW -p tcp --dport 80 -j ACCEPT
sudo iptables -I INPUT 6 -m state --state NEW -p tcp --dport 443 -j ACCEPT
sudo netfilter-persistent save
```

On Oracle Linux the equivalent is firewalld:

```bash
sudo firewall-cmd --permanent --add-service=http
sudo firewall-cmd --permanent --add-service=https
sudo firewall-cmd --reload
```

`iptables -L INPUT -n --line-numbers` must show your rules above the REJECT line.

---

## 3. Install Java 21 and nginx

```bash
sudo apt update
sudo apt install -y openjdk-21-jre-headless nginx unzip curl
java -version   # must report 21
```

---

## 4. Lay out the service

```bash
sudo useradd -r -m -d /srv/eaglerxhorizon -s /usr/sbin/nologin horizon
sudo mkdir -p /srv/eaglerxhorizon/{proxy,backend,web} /etc/eaglerxhorizon
sudo chown -R horizon:horizon /srv/eaglerxhorizon /etc/eaglerxhorizon
```

Copy into place:

- `EaglercraftXBungee.jar` → `/srv/eaglerxhorizon/proxy/`
- the Fabric 1.21.1 server (or `server.jar`) → `/srv/eaglerxhorizon/backend/`
- the built dashboard (`web/index.html`, `web/bootstrap.js`, `web/pkg/`) → `/srv/eaglerxhorizon/web/`
- `config/eaglerxbungee.yml` → `/srv/eaglerxhorizon/proxy/plugins/EaglercraftXBungee/config.yml`
- `config/server.properties.snippet` → merged into `/srv/eaglerxhorizon/backend/server.properties`

First boot generates the plugin's default config; port your values from the template
afterwards and restart once.

---

## 5. systemd unit

```bash
sudo cp deploy/systemd/eaglerxhorizon-proxy.service /etc/systemd/system/
sudo systemctl daemon-reload
sudo systemctl enable --now eaglerxhorizon-proxy
sudo journalctl -u eaglerxhorizon-proxy -f
```

Run the backend the same way (a second unit, or a `screen`/`tmux` session while you
are still iterating). The proxy exits if the backend is unreachable, which makes a
restart loop visible instead of silent.

---

## 6. TLS with nginx (wss://)

`deploy/nginx.conf` is a working server block; install it as
`/etc/nginx/sites-available/eaglerxhorizon` and:

```bash
sudo ln -s /etc/nginx/sites-available/eaglerxhorizon /etc/nginx/sites-enabled/
sudo rm -f /etc/nginx/sites-enabled/default
sudo nginx -t && sudo systemctl reload nginx
sudo certbot --nginx -d proxy.example.com     # or: caddy reverse-proxy
```

The two lines that actually make WebSockets work are the `Upgrade`/`Connection`
headers and a `proxy_read_timeout` longer than the client's idle window.

Prefer Caddy? One line:

```bash
caddy reverse-proxy --from proxy.example.com --to 127.0.0.1:8081
```

---

## 7. Verify

```bash
# 1. the listener is alive
ss -ltnp | grep -E ':(443|8081)'

# 2. the static dashboard answers
curl -sS -o /dev/null -w '%{http_code}\n' https://proxy.example.com/

# 3. the WebSocket handshake actually upgrades (101 is the only pass)
curl -sS -i -N \
  -H 'Connection: Upgrade' -H 'Upgrade: websocket' \
  -H 'Sec-WebSocket-Version: 13' -H 'Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==' \
  -H 'Origin: https://proxy.example.com' \
  --max-time 5 https://proxy.example.com/ws/ | head -n 3

# 4. the backend is reachable from the proxy host only
nc -z 127.0.0.1 25566 && echo backend-ok

# 5. full healthcheck with exit codes
./scripts/healthcheck.sh proxy.example.com 443
```

A `400`, `403` or `426` where you expect `101` is almost always the origin check or
a missing `Upgrade` header on the nginx side.

---

## 8. Operational notes

- **Logs**: `/var/log/nginx/access.log` for socket upgrades, journald for the proxy.
- **Memory**: the proxy is small (~400 MB); the backend JVM is the hog. Cap it with
  `-Xms1G -Xmx4G` in the unit file.
- **Idle drop**: browsers pause backgrounded tabs. `player-idle-timeout=0` plus a
  30 s read timeout is the combination that behaves.
- **Upgrades**: `scripts/build-wasm.sh` rebuilds the engine into `web/pkg/`, then
  `rsync` the `web/` directory. No restart needed for static files.
- **Backups**: back up `world/`, `plugins/`, `config/`, and `forwarding.secret`.
  Nothing else is stateful.
