# Cloudflare Tunnels Operator

Expose kubernetes service to internet with Cloudflare Tunnels using Ingress resources.

## Features

- Automatic tunnel creation
- Automatic dns record creation
- Use Ingress resource

## To Do

- [ ] Clean up if ingress is updated with different ingress

## Installation

Install using Helm

```shell
helm install  --repo https://mfadhlika.github.io/cloudflare-tunnels-operator -g cloudflare-tunnels-operator
```

## Usage

### Create Tunnel

Create `ClusterTunnel` resource

```yaml
apiVersion: cloudflare-tunnels-operator.io/v1alpha1
kind: ClusterTunnel
metadata:
  name: your-tunnel-name
spec:
  cloudflare:
    accountId: your-account-id
    zoneId: your-zone-id
    apiTokenSecretRef:
      name: cloudflare-credentials
      key: token
```

Create ingress for service

```yaml
apiVersion: networking.k8s.io/v1
kind: Ingress
metadata:
  name: whoami
  namespace: default
spec:
  rules:
    - host: whoami.example.com
      http:
        paths:
          - path: /
            pathType: Prefix
            backend:
              service:
                name: whoami
                port:
                  name: http
```
