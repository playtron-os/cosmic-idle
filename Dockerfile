ARG version=1.92
FROM rust:${version}
ARG version

RUN dpkg --add-architecture arm64
RUN apt-get update && apt-get install -y \
    libxkbcommon-dev \
    libxkbcommon-dev:arm64

RUN apt-get install -y \
    g++-aarch64-linux-gnu \
    libc6-dev-arm64-cross \
    git

# Taskfile support
RUN curl -1sLf 'https://dl.cloudsmith.io/public/task/task/setup.deb.sh' | bash
RUN apt-get install -y task

# RPM support
RUN apt-get install -y rpm librpmbuild10 elfutils

RUN rustup toolchain install 1.92.0 --profile minimal --component clippy,rustfmt
RUN rustup target add aarch64-unknown-linux-gnu --toolchain 1.92.0
RUN chmod -R 777 /usr/local/rustup

ENV CARGO_TARGET_AARCH64_UNKNOWN_LINUX_GNU_LINKER=aarch64-linux-gnu-gcc
ENV CC_aarch64_unknown_linux_gnu=aarch64-linux-gnu-gcc
ENV CXX_aarch64_unknown_linux_gnu=aarch64-linux-gnu-g++
