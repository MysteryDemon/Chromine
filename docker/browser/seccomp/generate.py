#!/usr/bin/env python3
import json
import sys

UNGATED = ["clone", "unshare", "chroot"]

def main(src: str) -> None:
    with open(src) as f:
        profile = json.load(f)
    rules = []
    for rule in profile["syscalls"]:
        names = [n for n in rule["names"] if n not in UNGATED]
        if not names:
            continue
        if names != rule["names"]:
            rule = dict(rule, names=names)
        rules.append(rule)
    rules.append({
        "names": UNGATED,
        "action": "SCMP_ACT_ALLOW",
        "comment": "chromine: Chrome's namespace sandbox (unprivileged user namespaces)",
    })
    profile["syscalls"] = rules
    json.dump(profile, sys.stdout, indent=2, sort_keys=False)
    sys.stdout.write("\n")

if __name__ == "__main__":
    if len(sys.argv) != 2:
        sys.exit("usage: generate.py DOCKER_DEFAULT_SECCOMP_JSON > seccomp-chrome.json")
    main(sys.argv[1])
