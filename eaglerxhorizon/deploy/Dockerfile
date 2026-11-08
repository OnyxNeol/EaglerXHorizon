# Base image for both the proxy and the backend in the local stack.
# Java 21 is the floor: EaglercraftXBungee and modern Fabric both require it.
FROM eclipse-temurin:21-jre-jammy

# curl for the healthcheck, unzip for dropping jars in, nc for port checks
RUN apt-get update \
 && apt-get install -y --no-install-recommends curl unzip netcat-openbsd ca-certificates \
 && rm -rf /var/lib/apt/lists/*

RUN useradd -r -m -d /srv/horizon -s /usr/sbin/nologin horizon \
 && mkdir -p /srv/proxy /srv/backend /srv/eaglerxhorizon/web \
 && chown -R horizon:horizon /srv

ENV JAVA_TOOL_OPTIONS="-Dfile.encoding=UTF-8" \
    HORIZON_BACKEND=backend:25566 \
    HORIZON_CLIENT_DIR=/srv/eaglerxhorizon/web

WORKDIR /srv
USER horizon

# Both services share this image; the compose file decides which jar runs.
# Mount the jars (or COPY them, for an air-gapped build) at:
#   /srv/proxy/EaglercraftXBungee.jar
#   /srv/backend/server.jar
EXPOSE 8081 25566

HEALTHCHECK --interval=20s --timeout=5s --start-period=60s --retries=6 \
  CMD curl -fsS http://127.0.0.1:8081/ >/dev/null || exit 1

CMD ["java", "-version"]
