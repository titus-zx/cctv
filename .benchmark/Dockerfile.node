FROM node:22-bookworm-slim
RUN apt-get update \
    && apt-get install -y --no-install-recommends ffmpeg rclone python3 ca-certificates tzdata \
    && rm -rf /var/lib/apt/lists/*
WORKDIR /app
COPY package.json server.js ./
COPY lib ./lib
COPY public ./public
COPY test ./test
COPY tools/discover.py ./tools/discover.py
ENV NODE_ENV=production HOST=0.0.0.0 PORT=3380 TZ=Asia/Jakarta \
    CONFIG_PATH=/app/runtime/config.json RCLONE_CONFIG=/app/rclone/rclone.conf
USER node
EXPOSE 3380
CMD ["node", "server.js"]
