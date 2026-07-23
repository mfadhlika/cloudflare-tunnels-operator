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

(Optional) Create `Secret` for your cloudflare tunnel

```yaml
apiVersion: v1
kind: Secret
type: Opaque
metadata:
    name: cloudflared-secret
stringData:
  credentials.json: |
    < insert credentials.json here >
  cert.pem: |
    < insert key here >
```

Create `ClusterTunnel` resource

```yaml
apiVersion: cloudflare-tunnels-operator.io/v1alpha1
kind: ClusterTunnel
metadata:
  name: your-tunnel-name
spec:
  # tunnelSecretRef is optional, if left empty, the controller will create one for you
  tunnelSecretRef:
    name: cloudflared-secret
    key: credentials.json
  # generate using `cloudflared tunnel login`
  originCertSecretRef:
    name: cloudflared-secret
    key: cert.pem
  # configure cloudflared options
  cloudflared:
    protocol: http2
    edgeIpVersion: '6'
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
