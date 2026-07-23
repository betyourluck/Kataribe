# Kataribe ノックサーバー (spec 23)。ビルド文脈 = knock/ ディレクトリ単体
# (workspace 外の独立クレート — engine のソースはイメージに入らない)。
# outcast の docker-compose.prod.yml が ghcr.io/betyourluck/kataribe-knock を pull する。

FROM rust:1-slim-bookworm AS build
WORKDIR /src
COPY Cargo.toml ./
COPY src ./src
RUN cargo build --release

FROM debian:bookworm-slim
COPY --from=build /src/target/release/kataribe-knock /usr/local/bin/kataribe-knock
# 書き込むファイルは無い (部屋台帳はメモリのみ) — 非 root で走らせる。
USER 65534:65534
EXPOSE 5000
CMD ["kataribe-knock"]
