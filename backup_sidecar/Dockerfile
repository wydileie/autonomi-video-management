FROM postgres:16

COPY backup-sidecar.sh /usr/local/bin/backup-sidecar

RUN chmod 755 /usr/local/bin/backup-sidecar

ENTRYPOINT ["/usr/local/bin/backup-sidecar"]
CMD ["schedule"]
