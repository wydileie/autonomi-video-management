# ── Stage 1: Build ────────────────────────────────────────────────────────────
FROM node:24-bookworm-slim AS build

WORKDIR /app

COPY package*.json ./
RUN npm ci

COPY index.html vite.config.mjs ./
COPY src ./src

# Build-time env vars injected from docker-compose build args
ARG REACT_APP_API_URL=/api
ARG REACT_APP_STREAM_URL=/stream

ENV REACT_APP_API_URL=$REACT_APP_API_URL
ENV REACT_APP_STREAM_URL=$REACT_APP_STREAM_URL

RUN npm run build

# ── Stage 2: Serve via nginx ───────────────────────────────────────────────────
FROM nginx:1.27-alpine

COPY --from=build /app/build /usr/share/nginx/html

# SPA fallback: all routes → index.html
RUN printf 'server {\n  listen 8080;\n  location / {\n    root /usr/share/nginx/html;\n    try_files $uri /index.html;\n  }\n}\n' \
    > /etc/nginx/conf.d/default.conf && \
    chown -R nginx:nginx /usr/share/nginx/html /etc/nginx/conf.d /var/cache/nginx /var/run

USER nginx

EXPOSE 8080
