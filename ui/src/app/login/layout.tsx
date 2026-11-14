import { GeistMono } from "geist/font/mono";
import { GeistSans } from "geist/font/sans";

import { cn } from "@/lib/utils";

export default function LoginLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return <div className={cn("pypen-theme", GeistSans.variable, GeistMono.variable)}>{children}</div>;
}
