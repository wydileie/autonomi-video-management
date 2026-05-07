# ── Stage 1: Build ────────────────────────────────────────────────────────────
FROM node:24-bookworm-slim AS build

WORKDIR /app

COPY package*.json ./
RUN npm ci

COPY index.html tsconfig.json vite.config.mts ./
COPY public ./public
COPY src ./src

# Build-time env vars injected from docker-compose build args
ARG REACT_APP_API_URL=/api
ARG REACT_APP_STREAM_URL=/stream

ENV REACT_APP_API_URL=$REACT_APP_API_URL
ENV REACT_APP_STREAM_URL=$REACT_APP_STREAM_URL

RUN npm run build

# ── Stage 2: Serve via nginx ───────────────────────────────────────────────────
FROM nginx:1.29-alpine

COPY --from=build /app/build /usr/share/nginx/html

# SPA fallback: all routes -> index.html
RUN apk upgrade --no-cache && \
    printf 'server {\n  listen 8080;\n  root /usr/share/nginx/html;\n\n  location = /index.html {\n    add_header Cache-Control "no-store" always;\n  }\n\n  location = /runtime-config.js {\n    add_header Cache-Control "no-store" always;\n  }\n\n  location ~* \\.(?:js|css|mjs|map|woff2?|ttf|otf|eot|svg|png|jpe?g|gif|webp|avif|ico)$ {\n    try_files $uri =404;\n    expires 1y;\n    add_header Cache-Control "public, immutable" always;\n  }\n\n  location / {\n    try_files $uri /index.html;\n  }\n}\n' \
    > /etc/nginx/conf.d/default.conf && \
    chown -R nginx:nginx /usr/share/nginx/html /etc/nginx/conf.d /var/cache/nginx /run /var/run

USER nginx

EXPOSE 8080
