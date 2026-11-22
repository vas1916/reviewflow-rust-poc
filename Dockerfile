# Multistage build: no Rust compiler in runtime container.
FROM rust:1-bookworm AS build
WORKDIR /app
COPY Cargo.toml Cargo.lock ./
COPY src ./src
COPY static ./static
RUN cargo build --release --locked

FROM debian:bookworm-slim
RUN useradd -r -u 10001 reviewflow && mkdir /data && chown reviewflow:reviewflow /data
COPY --from=build /app/target/release/reviewflow /usr/local/bin/reviewflow
# Workday request template (workday/request-template.xml once your Workday team provides it).
WORKDIR /app
COPY workday ./workday
USER reviewflow
ENV SQLITE_PATH=/data/reviewflow.db BIND_ADDR=0.0.0.0:8080 AI_MODE=rules     WORKDAY_REQUEST_TEMPLATE=/app/workday/request-template.xml
EXPOSE 8080
VOLUME /data
CMD ["reviewflow"]
