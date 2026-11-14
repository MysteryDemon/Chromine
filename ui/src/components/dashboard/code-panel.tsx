"use client";

export function codeUrl(code: { url: string | null; port: number }) {
  return code.url ?? `${window.location.protocol}//${window.location.hostname}:${code.port}/`;
}

export function CodePanel({ src }: { src: string }) {
  return (
    <iframe
      title="VS Code"
      src={src}
      allow="clipboard-read; clipboard-write"
      className="block h-full w-full border-0 bg-black"
    />
  );
}
