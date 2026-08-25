FROM debian:bookworm AS build
RUN apt-get update && apt-get install --yes --no-install-recommends gcc libc6-dev
COPY ptrace-kubernetes-sidecar-probe.c /src/probe.c
RUN gcc -O2 -Wall -Wextra -Werror -o /sidecar-probe /src/probe.c

FROM debian:bookworm-slim
COPY --from=build /sidecar-probe /sidecar-probe
ENTRYPOINT ["/sidecar-probe"]
