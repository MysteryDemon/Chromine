```
chromine/
├── crates/
│   ├── agent/
│   │   ├── src/
│   │   │   ├── audio.rs
│   │   │   ├── chrome.rs
│   │   │   ├── config.rs
│   │   │   ├── display.rs
│   │   │   ├── downloads.rs
│   │   │   ├── history.rs
│   │   │   ├── lib.rs
│   │   │   ├── main.rs
│   │   │   ├── native.rs
│   │   │   ├── prefs.rs
│   │   │   └── video.rs
│   │   └── Cargo.toml
│   ├── shared/
│   │   ├── src/
│   │   │   ├── fsutil.rs
│   │   │   └── lib.rs
│   │   └── Cargo.toml
│   └── worker/
│       ├── migrations/
│       │   ├── 0001_init.sql
│       │   ├── 0002_notify_kind.sql
│       │   ├── 0003_source_url.sql
│       │   └── 0004_drop_notifications.sql
│       ├── src/
│       │   ├── auth.rs
│       │   ├── cleanup.rs
│       │   ├── config.rs
│       │   ├── db.rs
│       │   ├── filecloud.rs
│       │   ├── ingest.rs
│       │   ├── lib.rs
│       │   ├── main.rs
│       │   ├── redact.rs
│       │   ├── ui.rs
│       │   ├── upload.rs
│       │   └── web.rs
│       └── Cargo.toml
├── docker/
│   ├── browser/
│   │   ├── download-monitor/
│   │   │   ├── manifest.json
│   │   │   └── service-worker.js
│   │   ├── extensions/
│   │   │   └── ddkjiahejlhfcafbddmgiahcphecmpfh.json
│   │   ├── openbox/
│   │   │   └── menu.xml
│   │   ├── seccomp/
│   │   │   └── generate.py
│   │   ├── video-downloader/
│   │   │   ├── background.js
│   │   │   ├── core.js
│   │   │   ├── floating.js
│   │   │   ├── manifest.json
│   │   │   └── player.js
│   │   ├── Dockerfile
│   │   ├── pack-crx.py
│   │   └── seccomp-chrome.json
│   └── worker/
│       └── Dockerfile
├── scripts/
│   └── check-deployment.py
├── ui/
│   ├── public/
│   │   ├── icons/
│   │   │   ├── apple-touch-icon.png
│   │   │   ├── icon-192.png
│   │   │   ├── icon-512.png
│   │   │   ├── icon.svg
│   │   │   └── maskable-512.png
│   │   └── manifest.webmanifest
│   ├── src/
│   │   ├── app/
│   │   │   ├── login/
│   │   │   │   ├── layout.tsx
│   │   │   │   └── page.tsx
│   │   │   ├── globals.css
│   │   │   ├── layout.tsx
│   │   │   └── page.tsx
│   │   ├── components/
│   │   │   ├── dashboard/
│   │   │   │   ├── alerts.tsx
│   │   │   │   ├── browser-panel.tsx
│   │   │   │   ├── dashboard.tsx
│   │   │   │   ├── state-badge.tsx
│   │   │   │   ├── top-bar.tsx
│   │   │   │   └── transfers-panel.tsx
│   │   │   ├── pypen/
│   │   │   │   ├── alert.tsx
│   │   │   │   ├── button.tsx
│   │   │   │   ├── card.tsx
│   │   │   │   ├── input.tsx
│   │   │   │   └── label.tsx
│   │   │   └── ui/
│   │   │       ├── alert-dialog.tsx
│   │   │       ├── button.tsx
│   │   │       ├── sonner.tsx
│   │   │       ├── tabs.tsx
│   │   │       └── tooltip.tsx
│   │   └── lib/
│   │       ├── api.ts
│   │       ├── format.ts
│   │       ├── remote-audio.ts
│   │       └── utils.ts
│   ├── components.json
│   ├── next.config.ts
│   ├── package-lock.json
│   ├── package.json
│   ├── postcss.config.mjs
│   └── tsconfig.json
├── .dockerignore
├── .env.example
├── .gitignore
├── Cargo.lock
├── Cargo.toml
├── docker-compose.yml
├── README.md
└── rustfmt.toml
```
