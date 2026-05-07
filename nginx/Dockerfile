FROM nginx:1.29-alpine

COPY conf.d/default.conf /etc/nginx/conf.d/default.conf

RUN apk upgrade --no-cache && \
    chown -R nginx:nginx /etc/nginx/conf.d /var/cache/nginx /run /var/run
