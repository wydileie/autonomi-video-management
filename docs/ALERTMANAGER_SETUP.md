# Alertmanager Setup

The checked-in Alertmanager config intentionally uses an empty `default`
receiver. It lets the monitoring stack start everywhere without sending test
alerts to a real on-call channel.

For production, edit `monitoring/alertmanager/alertmanager.yml` on the deploy
host or use a private Compose override that mounts a host-local config file.
Do not commit real webhook URLs, SMTP passwords, or routing keys.

## Slack

Set this in `.env.production` or your secret manager:

```dotenv
ALERTMANAGER_SLACK_WEBHOOK_URL=https://hooks.slack.com/services/...
```

Use this receiver shape, replacing the webhook placeholder before mounting the
file:

```yaml
route:
  receiver: slack
  group_by:
    - alertname
    - service
    - job
  group_wait: 30s
  group_interval: 5m
  repeat_interval: 4h

receivers:
  - name: slack
    slack_configs:
      - api_url: "https://hooks.slack.com/services/..."
        channel: "#alerts"
        title: "{{ .CommonLabels.alertname }} {{ .CommonLabels.severity }}"
        text: "{{ range .Alerts }}{{ .Annotations.summary }}\n{{ .Annotations.description }}\n{{ end }}"
        send_resolved: true
```

## Email

Set these values privately:

```dotenv
ALERTMANAGER_SMTP_SMARTHOST=smtp.example.com:587
ALERTMANAGER_SMTP_FROM=alerts@example.com
ALERTMANAGER_SMTP_USERNAME=alerts@example.com
ALERTMANAGER_SMTP_PASSWORD=change-me
ALERTMANAGER_EMAIL_TO=ops@example.com
```

Use this receiver shape:

```yaml
global:
  smtp_smarthost: smtp.example.com:587
  smtp_from: alerts@example.com
  smtp_auth_username: alerts@example.com
  smtp_auth_password: change-me
  smtp_require_tls: true

route:
  receiver: email
  group_by:
    - alertname
    - service
    - job
  group_wait: 30s
  group_interval: 5m
  repeat_interval: 4h

receivers:
  - name: email
    email_configs:
      - to: ops@example.com
        send_resolved: true
```

## PagerDuty

Set the routing key privately:

```dotenv
ALERTMANAGER_PAGERDUTY_ROUTING_KEY=<integration-routing-key>
```

Use this receiver shape:

```yaml
route:
  receiver: pagerduty
  group_by:
    - alertname
    - service
    - job
  group_wait: 30s
  group_interval: 5m
  repeat_interval: 4h

receivers:
  - name: pagerduty
    pagerduty_configs:
      - routing_key: "<integration-routing-key>"
        description: "{{ .CommonLabels.alertname }} {{ .CommonLabels.severity }}"
        send_resolved: true
```

## Validate And Reload

Validate the merged Compose config before starting:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.monitoring.yml \
  config
```

Validate Alertmanager config inside the running container:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.monitoring.yml \
  exec alertmanager amtool check-config /etc/alertmanager/alertmanager.yml
```

Restart Alertmanager after changing the mounted file:

```bash
docker compose --env-file .env.production \
  -f docker-compose.yml \
  -f docker-compose.prod.yml \
  -f docker-compose.monitoring.yml \
  restart alertmanager
```
