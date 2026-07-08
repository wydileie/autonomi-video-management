FROM alpine:3.24

RUN apk add --no-cache bash coreutils findutils sqlite

COPY backup-sidecar.sh /usr/local/bin/backup-sidecar

RUN chmod 755 /usr/local/bin/backup-sidecar

ENTRYPOINT ["/usr/local/bin/backup-sidecar"]
CMD ["schedule"]
