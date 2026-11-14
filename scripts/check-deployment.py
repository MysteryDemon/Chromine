#!/usr/bin/env python3
import json
import os
import re
import subprocess
import sys
import tempfile

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
failures = []

def check(cond, msg):
    print(("ok   " if cond else "FAIL ") + msg)
    if not cond:
        failures.append(msg)

def compose_config(extra_files=()):
    env = (
        "APP_AUTH_USER=admin\n"
        "APP_AUTH_PASSWORD='example password 1234'\n"
    )
    with tempfile.NamedTemporaryFile("w", suffix=".env", delete=False) as f:
        f.write(env)
        env_path = f.name
    try:
        cmd = ["docker", "compose", "--project-directory", ROOT, "--env-file", env_path]
        for x in ("docker-compose.yml",) + tuple(extra_files):
            cmd += ["-f", os.path.join(ROOT, x)]
        out = subprocess.run(cmd + ["config", "--format", "json"], capture_output=True, text=True, check=True)
        return json.loads(out.stdout)
    finally:
        os.unlink(env_path)

def main():
    cfg = compose_config()
    services = cfg["services"]
    check(set(services) == {"browser", "worker", "code"}, "exactly three services: browser, worker, code")

    for name, s in services.items():
        if name == "code":
            continue
        check(not s.get("privileged", False), f"{name}: not privileged")
        check(not s.get("cap_add"), f"{name}: no added capabilities")
        check("ALL" in (s.get("cap_drop") or []), f"{name}: drops all capabilities")
        check("no-new-privileges:true" in (s.get("security_opt") or []), f"{name}: no-new-privileges")
        user = str(s.get("user", ""))
        check(user and not user.startswith("0") and not user.startswith("root"), f"{name}: runs as non-root user ({user})")
        check(s.get("read_only") is True, f"{name}: read-only root filesystem")
        check("healthcheck" in s, f"{name}: has a health check")
        check(s.get("restart") == "unless-stopped", f"{name}: restart policy")
        for v in s.get("volumes", []):
            src = str(v.get("source", ""))
            check("docker.sock" not in src, f"{name}: does not mount the Docker socket ({src})")
            if v.get("type") == "bind":
                check(not src.startswith(("/etc", "/var/run", "/proc", "/sys", "/root")) and src != "/", f"{name}: no host system bind mounts ({src})")
        check(not s.get("network_mode"), f"{name}: no host network mode")

    code = services["code"]
    check(not code.get("privileged", False) and not code.get("cap_add"), "code: not privileged, no added capabilities")
    check(not code.get("ports"), "code publishes no ports")
    check(code.get("network_mode") == "service:browser", "code shares only the browser's network")
    code_vols = {v.get("target"): v.get("source") for v in code.get("volumes", [])}
    check(not any("docker.sock" in str(src) for src in code_vols.values()), "code does not mount the Docker socket")
    check(not {"state", "downloads", "vnc", "chrome-profile"} & set(code_vols.values()), "code cannot reach worker or browser data")
    check(code_vols.get("/code-socket") == "code-socket" and code_vols.get("/code-links") == "code-links", "code shares only its socket and link folders")

    check(not services["browser"].get("ports"), "browser publishes no ports")
    ports = services["worker"].get("ports", [])
    check(len(ports) == 1 and ports[0].get("host_ip") == "127.0.0.1", "dashboard binds to 127.0.0.1 by default")
    check(all(str(p.get("target")) in ("8000", "8001", "6758", "8080") for p in ports), "worker publishes only the dashboard, VS Code, WebRTC and certificate-check listeners")

    nets = {k: set((v.get("networks") or {}).keys()) for k, v in services.items()}
    check(not (nets["browser"] & nets["worker"]), "browser and worker share no network")
    for name in ("browser", "worker"):
        vols = {v.get("target"): v.get("source") for v in services[name].get("volumes", [])}
        check(vols.get("/vnc") == "vnc", f"{name} mounts the VNC socket volume at /vnc")

    b = services["browser"]
    check(any(o.startswith("seccomp=") and "seccomp-chrome.json" in o for o in b.get("security_opt", [])), "browser uses the Chrome seccomp profile")
    check(int(b.get("shm_size", 0)) >= 1 << 30, "browser /dev/shm is at least 1 GiB")
    exposed = json.dumps(services)
    for port in ("5900", "9222", "6080:"):
        check(f'"published": "{port.rstrip(":")}"' not in exposed, f"port {port.rstrip(':')} is not published")

    with open(os.path.join(ROOT, "docker/browser/seccomp-chrome.json")) as f:
        prof = json.load(f)
    check(prof["defaultAction"] == "SCMP_ACT_ERRNO", "seccomp default action is deny")
    extra = [r for r in prof["syscalls"] if "chromine" in r.get("comment", "")]
    check(len(extra) == 1 and sorted(extra[0]["names"]) == ["chroot", "clone", "unshare"], "seccomp adds only clone/unshare/chroot")
    for forbidden in ("mount", "setns", "bpf", "ptrace", "kexec_load"):
        allowed = [r for r in prof["syscalls"] if forbidden in r["names"] and r["action"] == "SCMP_ACT_ALLOW" and not r.get("includes")]
        check(not allowed, f"seccomp does not allow {forbidden} unconditionally")

    with open(os.path.join(ROOT, "crates/agent/src/display.rs")) as f:
        display_rs = f.read()
    check('"-rfbport".into(),\n        "-1".into()' in display_rs, "VNC server has no TCP listener")
    check('"-rfbunixpath".into()' in display_rs and '"0660".into()' in display_rs, "VNC server listens on a group-only Unix socket")
    check('"-UseBlacklist=0".into()' in display_rs, "VNC server does not lock viewers out after aborted connections")
    with open(os.path.join(ROOT, "crates/agent/src/lib.rs")) as f:
        check("UnixStream::connect(&cfg.vnc_socket)" not in f.read(), "agent health check does not open VNC connections")
    with open(os.path.join(ROOT, "crates/agent/src/audio.rs")) as f:
        audio_rs = f.read()
    check("module-native-protocol-unix" in audio_rs
          and not re.search(r"module-(native-protocol-tcp|esound|zeroconf|rtp|simple-protocol|http|tunnel)", audio_rs),
          "sound server listens only on a private Unix socket")
    check("from_mode(0o660)" in audio_rs, "sound socket is group-only")
    with open(os.path.join(ROOT, "crates/worker/src/ui.rs")) as f:
        ui_rs = f.read()
    public_fn = " ".join(ui_rs[ui_rs.index("pub fn is_public"):].split("}", 1)[0].split())
    with open(os.path.join(ROOT, "crates/worker/src/web.rs")) as f:
        web_rs = f.read()
    public_routes = re.findall(r'\.route\("([^"]+)"', web_rs[web_rs.index("Router::new()\n        .route(\"/healthz\""):web_rs.index(".merge(protected)")])
    check('path == "/login" || path == "/manifest.webmanifest" || path.starts_with("/_next/static/") || path.starts_with("/icons/")' in public_fn
          and public_routes == ["/healthz", "/login", "/_next/static/{*path}", "/manifest.webmanifest", "/icons/{*path}"],
          "only the login page, build assets and PWA manifest/icons are public")

    ui_src = os.path.join(ROOT, "ui", "src")
    sinks = ("dangerouslySetInnerHTML", "innerHTML", "outerHTML", "insertAdjacentHTML", "document.write", "eval(")
    hits = []
    for dirpath, _, files in os.walk(ui_src):
        for fn in files:
            if fn.endswith((".ts", ".tsx")):
                with open(os.path.join(dirpath, fn)) as f:
                    text = f.read()
                hits += [f"{fn}: {s}" for s in sinks if s in text]
    check(not hits, f"UI source uses no raw-HTML sinks {hits or ''}")
    with open(os.path.join(ROOT, "ui", "next.config.ts")) as f:
        check('output: "export"' in f.read(), "UI is a static export (no Node.js at run time)")
    check(os.path.exists(os.path.join(ROOT, "ui", "package-lock.json")), "UI lockfile committed")

    with open(os.path.join(ROOT, "docker/browser/Dockerfile")) as f:
        bdf = f.read()
    check("/etc/opt/chrome/policies" not in bdf, "no Chrome enterprise policy installed")
    with open(os.path.join(ROOT, "crates/agent/src/prefs.rs")) as f:
        prefs = f.read()
    check('"prompt_for_download"], json!(false)' in prefs, "agent sets the download folder without a save prompt")
    ext = os.path.join(ROOT, "docker/browser/extensions/ddkjiahejlhfcafbddmgiahcphecmpfh.json")
    with open(ext) as f:
        check(json.load(f).get("external_update_url") == "https://clients2.google.com/service/update2/crx", "uBlock Origin Lite pre-installed from the Chrome Web Store")
    check("COPY docker/browser/extensions/ /opt/google/chrome/extensions/" in bdf, "browser image installs the extension files")
    check(services["browser"]["environment"].get("CHROME_SAFE_BROWSING") == "off", "Safe Browsing setting passed to the browser (default off)")

    with open(os.path.join(ROOT, "crates/agent/src/chrome.rs")) as f:
        chrome_rs = f.read()
    args_fn = chrome_rs[chrome_rs.index("pub fn chrome_args"):chrome_rs.index("pub fn clear_stale_profile_lock")]
    check(not re.search(r"remote-debugging|enable-automation|AutomationControlled", args_fn), "agent starts Chrome without remote debugging or automation switches")
    check('"--start-maximized"' in args_fn and "--window-size" not in args_fn and '"--hide-scrollbars"' in args_fn,
          "Chrome starts maximized (no fixed window size) and without page scrollbars")
    check("/etc/opt/chrome/native-messaging-hosts/com.chromine.agent.json" in bdf, "browser image registers the agent as native messaging host")
    check("/opt/chromine/chromine-download-monitor.crx" in bdf, "browser image installs the download-monitor extension")
    with open(os.path.join(ROOT, "docker/browser/download-monitor/manifest.json")) as f:
        ext_manifest = json.load(f)
    check(sorted(ext_manifest.get("permissions", [])) == ["downloads", "nativeMessaging"] and "host_permissions" not in ext_manifest,
          "download monitor asks only for downloads + nativeMessaging")
    check("/etc/opt/chrome/native-messaging-hosts/com.chromine.video.json" in bdf, "browser image registers the video native messaging host")
    check("/opt/chromine/chromine-video-downloader.crx" in bdf, "browser image installs the video-downloader extension")
    check("ln -s chromine-agent /usr/local/bin/chromine-video-host" in bdf, "video host is the agent binary")
    with open(os.path.join(ROOT, "docker/browser/video-downloader/manifest.json")) as f:
        video_manifest = json.load(f)
    check(sorted(video_manifest.get("permissions", [])) == ["nativeMessaging", "storage"] and "host_permissions" not in video_manifest,
          "video downloader asks only for nativeMessaging + storage")
    check(re.search(r"^\s+ffmpeg \\$", bdf, re.M) is not None, "browser image installs ffmpeg")
    check(re.search(r"^\s+pulseaudio \\$", bdf, re.M) is not None, "browser image installs PulseAudio for sound")
    check("gpgv --keyring ./ytdlp.gpg SHA2-256SUMS.sig SHA2-256SUMS" in bdf and "sha256sum -c" in bdf,
          "yt-dlp release is signature- and checksum-verified")
    check('"$base/yt-dlp_linux.zip"' in bdf and "unzip -q yt-dlp_linux.zip -d /opt/yt-dlp" in bdf and '"$base/yt-dlp_linux"' not in bdf,
          "yt-dlp is the unpacked build (the self-extracting one cannot start with a noexec /tmp)")
    with open(os.path.join(ROOT, "crates/agent/src/video.rs")) as f:
        video_rs = f.read()
    check("--exec" not in video_rs and "--ignore-config" in video_rs, "yt-dlp runs without config files or --exec")

    with open(os.path.join(ROOT, ".env.example")) as f:
        envx = f.read()
    check(re.search(r"^APP_AUTH_PASSWORD=\s*$", envx, re.M) is not None, ".env.example leaves APP_AUTH_PASSWORD empty")

    print()
    if failures:
        print(f"{len(failures)} check(s) failed")
        sys.exit(1)
    print("all deployment checks passed")

if __name__ == "__main__":
    main()
