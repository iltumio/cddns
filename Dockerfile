# Build stage
FROM rust:1-alpine AS builder

# Install build dependencies
RUN apk add --no-cache musl-dev openssl-dev openssl-libs-static pkgconfig

WORKDIR /app

# Copy manifests first for better caching
COPY Cargo.toml Cargo.lock* ./

# Create a dummy src to build dependencies
RUN mkdir src && \
    echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src

# Copy actual source code
COPY src ./src

# Build the application (touch to invalidate the dummy)
RUN touch src/main.rs && \
    cargo build --release --locked && \
    strip /app/target/release/cddns

# Runtime stage - using scratch for minimal size
FROM scratch

# Copy CA certificates for HTTPS
COPY --from=builder /etc/ssl/certs/ca-certificates.crt /etc/ssl/certs/

# Copy the binary
COPY --from=builder /app/target/release/cddns /cddns

# Create a volume mount point for config
VOLUME ["/config"]

# Set the working directory
WORKDIR /config

# Run the service by default
ENTRYPOINT ["/cddns"]
CMD ["service", "-c", "/config/config.toml"]
