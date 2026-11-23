#!/usr/bin/env python3
"""Render the Cloud Run worker pool manifest from deploy/config.toml.

Usage: render_workerpool.py <config.toml> <image_tag>   -> YAML on stdout
       render_workerpool.py <config.toml> --get <section.key>  -> one value
"""
import json
import sys
import tomllib


def load(path):
    with open(path, "rb") as f:
        return tomllib.load(f)


def image(c, name, tag):
    g = c["gcp"]
    return f"{g['region']}-docker.pkg.dev/{g['project']}/{g['artifact_repo']}/{name}:{tag}"


def env(d):
    return [{"name": k, "value": str(v)} for k, v in d.items()]


def manifest(c, tag):
    g, wp, s, gw, hl = c["gcp"], c["worker_pool"], c["secrets"], c["gateway"], c["venue_hl"]
    if int(wp["instances"]) != 1:
        sys.exit("worker_pool.instances must be 1 (one Telegram poller per token)")
    if not str(gw["telegram_owner_id"]).isdigit():
        sys.exit("gateway.telegram_owner_id must be numeric")

    volumes = [{"name": "telegram-token", "secret": {"secretName": s["telegram_token"],
                                                      "items": [{"key": "latest", "path": "token"}]}}]
    gateway = {
        "name": "gateway",
        "image": image(c, "gateway", tag),
        "env": env({
            "VENUES": "hl=http://127.0.0.1:50051",
            "TELEGRAM_OWNER_ID": gw["telegram_owner_id"],
            "TELEGRAM_TOKEN_FILE": "/run/secrets/telegram/token",
            "RUST_LOG": gw["rust_log"],
        }),
        "resources": {"limits": {"cpu": str(gw["cpu"]), "memory": gw["memory"]}},
        "volumeMounts": [{"name": "telegram-token", "mountPath": "/run/secrets/telegram", "readOnly": True}],
    }
    hl_env = {
        "HL_ADDRESS": hl["address"],
        "HL_TESTNET": str(hl["testnet"]).lower(),
        "VENUE_LISTEN_ADDR": "127.0.0.1:50051",
        "POLL_SECS": hl["poll_secs"],
        "LIQ_ALERT_PCT": hl["liq_alert_pct"],
        "FUNDING_ALERT_APR_PCT": hl["funding_alert_apr_pct"],
        "ALERT_REPEAT_MINS": hl["alert_repeat_mins"],
        "RUST_LOG": hl["rust_log"],
    }
    venue = {
        "name": "venue-hl",
        "image": image(c, "venue-hl", tag),
        "env": env(hl_env),
        "resources": {"limits": {"cpu": str(hl["cpu"]), "memory": hl["memory"]}},
    }
    if s.get("hl_agent_key"):
        volumes.append({"name": "hl-agent-key", "secret": {"secretName": s["hl_agent_key"],
                                                            "items": [{"key": "latest", "path": "key"}]}})
        venue["env"].append({"name": "HL_AGENT_KEY_FILE", "value": "/run/secrets/hl/key"})
        venue["volumeMounts"] = [{"name": "hl-agent-key", "mountPath": "/run/secrets/hl", "readOnly": True}]

    return {
        "apiVersion": "run.googleapis.com/v1",
        "kind": "WorkerPool",
        "metadata": {
            "name": wp["name"],
            "labels": {"cloud.googleapis.com/location": g["region"]},
            "annotations": {
                "run.googleapis.com/launch-stage": "BETA",
                "run.googleapis.com/scalingMode": "manual",
                "run.googleapis.com/manualInstanceCount": str(wp["instances"]),
            },
        },
        "spec": {
            "template": {
                "metadata": {"annotations": {"run.googleapis.com/execution-environment": "gen2"}},
                "spec": {
                    "serviceAccountName": f"{g['service_account']}@{g['project']}.iam.gserviceaccount.com",
                    "containers": [gateway, venue],
                    "volumes": volumes,
                },
            }
        },
    }


def main():
    if len(sys.argv) != 3 and not (len(sys.argv) == 4 and sys.argv[2] == "--get"):
        sys.exit(__doc__)
    c = load(sys.argv[1])
    if sys.argv[2] == "--get":
        sec, key = sys.argv[3].split(".", 1)
        print(c[sec][key])
        return
    # JSON is valid YAML; avoids a PyYAML dependency.
    print(json.dumps(manifest(c, sys.argv[2]), indent=2))


if __name__ == "__main__":
    main()
