#!/bin/sh
# Throwaway PostgreSQL and MySQL servers with TLS, for the live TLS tests
# (`cargo test live_tls`): a certificate authority of their own, a server
# certificate for `localhost` only (so 127.0.0.1 fails verify-full), a
# second authority that signed nothing (the wrong CA), and a client
# certificate for a user who logs in only with it (`podelis_cert`).
#
#   scripts/tls-test-servers.sh [dir]      # default target/tls-test
#
# Then, as it prints:
#   PODELIS_TEST_TLS_DIR=… PODELIS_TEST_TLS_PG_PORT=55443 PODELIS_TEST_TLS_MYSQL_PORT=53443 cargo test live_tls
#
# `scripts/tls-test-servers.sh stop` removes the containers.
set -eu

pg=podelis-tls-pg
my=podelis-tls-mysql
if [ "${1:-}" = stop ]; then
    docker rm -f "$pg" "$my" >/dev/null 2>&1 || true
    exit 0
fi

dir=$(mkdir -p "${1:-target/tls-test}" && cd "${1:-target/tls-test}" && pwd)
cd "$dir"

# An explicit config: LibreSSL (macOS) and OpenSSL make the same
# certificates from it (a CA's basicConstraints, the names, the uses).
cat > openssl.cnf <<'EOF'
[req]
distinguished_name = dn
prompt = no
[dn]
CN = unused
[ca]
basicConstraints = critical, CA:TRUE
keyUsage = critical, keyCertSign, cRLSign
subjectKeyIdentifier = hash
[server]
basicConstraints = CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = serverAuth
subjectAltName = DNS:localhost
authorityKeyIdentifier = keyid
[client]
basicConstraints = CA:FALSE
keyUsage = critical, digitalSignature, keyEncipherment
extendedKeyUsage = clientAuth
authorityKeyIdentifier = keyid
EOF
quiet() { "$@" >/dev/null 2>&1; }
authority() { # name, subject
    quiet openssl req -x509 -config openssl.cnf -extensions ca -newkey rsa:2048 -nodes -days 30 \
        -subj "/CN=$2" -keyout "$1.key" -out "$1.crt"
}
signed() { # name, subject, extensions
    quiet openssl req -config openssl.cnf -newkey rsa:2048 -nodes -subj "/CN=$2" -keyout "$1.key" -out "$1.csr"
    quiet openssl x509 -req -in "$1.csr" -CA ca.crt -CAkey ca.key -CAcreateserial -days 30 \
        -extfile openssl.cnf -extensions "$3" -out "$1.crt"
}
authority ca "Podelis test CA"
authority other-ca "Another CA"
signed server localhost server
signed client podelis_cert client
# PKCS#8 for the client's key, which every TLS library reads.
quiet openssl pkcs8 -topk8 -nocrypt -in client.key -out client.pk8 && mv client.pk8 client.key
chmod 644 ./*.crt ./*.key

# The user who logs in only with the client certificate.
cat > pg-init.sh <<'EOF'
#!/bin/bash
set -e
psql -v ON_ERROR_STOP=1 -U postgres -c "CREATE ROLE podelis_cert LOGIN"
sed -i '1i hostssl all podelis_cert all cert' "$PGDATA/pg_hba.conf"
EOF
cat > mysql-init.sql <<'EOF'
CREATE USER 'podelis_cert'@'%' IDENTIFIED BY 'certpw' REQUIRE X509;
EOF

# The servers copy the certificates in, their key owned by them and
# readable by them only (PostgreSQL refuses a key others can read).
docker rm -f "$pg" "$my" >/dev/null 2>&1 || true
docker run -d --name "$pg" -e POSTGRES_PASSWORD=postgres -p 127.0.0.1:55443:5432 -v "$dir:/certs:ro" \
    --entrypoint bash postgres:17 -c '
        mkdir -p /tls && cp /certs/server.crt /certs/server.key /certs/ca.crt /tls/ \
        && chown postgres /tls/* && chmod 600 /tls/server.key \
        && cp /certs/pg-init.sh /docker-entrypoint-initdb.d/ \
        && exec docker-entrypoint.sh postgres -c ssl=on -c ssl_cert_file=/tls/server.crt \
             -c ssl_key_file=/tls/server.key -c ssl_ca_file=/tls/ca.crt' >/dev/null
docker run -d --name "$my" -e MYSQL_ROOT_PASSWORD=root -p 127.0.0.1:53443:3306 -v "$dir:/certs:ro" \
    --entrypoint bash mysql:8.4 -c '
        mkdir -p /tls && cp /certs/server.crt /certs/server.key /certs/ca.crt /tls/ \
        && chown mysql /tls/* && chmod 600 /tls/server.key \
        && cp /certs/mysql-init.sql /docker-entrypoint-initdb.d/ \
        && exec docker-entrypoint.sh mysqld --ssl-ca=/tls/ca.crt --ssl-cert=/tls/server.crt \
             --ssl-key=/tls/server.key' >/dev/null

# Ready over TCP (an init server answers on its socket first).
for _ in $(seq 1 90); do
    docker exec "$pg" pg_isready -h 127.0.0.1 -U postgres >/dev/null 2>&1 \
        && docker exec "$my" mysql -h127.0.0.1 -uroot -proot -e 'SELECT 1' >/dev/null 2>&1 && break
    sleep 2
done
docker exec "$pg" pg_isready -h 127.0.0.1 -U postgres >/dev/null
docker exec "$my" mysql -h127.0.0.1 -uroot -proot -e 'SELECT 1' >/dev/null 2>&1

echo "PODELIS_TEST_TLS_DIR=$dir PODELIS_TEST_TLS_PG_PORT=55443 PODELIS_TEST_TLS_MYSQL_PORT=53443"
