FROM nginx:1.27-alpine

COPY conf.d/default.conf /etc/nginx/conf.d/default.conf

RUN chown -R nginx:nginx /etc/nginx/conf.d /var/cache/nginx /var/run
