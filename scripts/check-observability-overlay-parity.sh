#!/usr/bin/env bash
set -euo pipefail

image_for_service() {
  local file="$1"
  local service="$2"

  awk -v service="$service" '
    $0 ~ "^[[:space:]][[:space:]]" service ":[[:space:]]*$" {
      in_service = 1
      next
    }
    in_service && /^[[:space:]][[:space:]][A-Za-z0-9_-]+:[[:space:]]*$/ {
      in_service = 0
    }
    in_service && /^[[:space:]][[:space:]][[:space:]][[:space:]]image:[[:space:]]*/ {
      sub(/^[[:space:]][[:space:]][[:space:]][[:space:]]image:[[:space:]]*/, "")
      print
      found = 1
      exit
    }
    END {
      if (!found) {
        exit 1
      }
    }
  ' "$file"
}

check_image_pin() {
  local service="$1"
  local split_file="$2"
  local combined_file="$3"
  local split_image
  local combined_image

  if ! split_image="$(image_for_service "$split_file" "$service")"; then
    echo "Could not find image pin for ${service} in ${split_file}" >&2
    exit 1
  fi
  if ! combined_image="$(image_for_service "$combined_file" "$service")"; then
    echo "Could not find image pin for ${service} in ${combined_file}" >&2
    exit 1
  fi

  if [[ "$split_image" != "$combined_image" ]]; then
    echo "Observability image pin drift for ${service}:" >&2
    echo "  ${split_file}: ${split_image}" >&2
    echo "  ${combined_file}: ${combined_image}" >&2
    exit 1
  fi
}

combined_file="docker-compose.observability.yml"

check_image_pin "prometheus" "docker-compose.monitoring.yml" "$combined_file"
check_image_pin "node_exporter" "docker-compose.monitoring.yml" "$combined_file"
check_image_pin "alertmanager" "docker-compose.monitoring.yml" "$combined_file"
check_image_pin "grafana" "docker-compose.monitoring.yml" "$combined_file"
check_image_pin "grafana" "docker-compose.logging.yml" "$combined_file"
check_image_pin "loki" "docker-compose.logging.yml" "$combined_file"
check_image_pin "promtail" "docker-compose.logging.yml" "$combined_file"
