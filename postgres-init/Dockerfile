FROM postgres:16

COPY init-dbs.sh /docker-entrypoint-initdb.d/init-dbs.sh

RUN chmod 755 /docker-entrypoint-initdb.d/init-dbs.sh
