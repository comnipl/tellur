import {
  AreaChart,
  CircleCheck,
  LoaderCircle,
  TriangleAlert,
} from "lucide-react";
import type { ServerInfo } from "../types";

type CompileStatus = ServerInfo["compileStatus"] | "disconnected";

interface HeaderProps {
  projectName: string;
  url: string;
  compileStatus: CompileStatus;
  compileError: string | null;
}

export function Header({
  projectName,
  url,
  compileStatus,
  compileError,
}: HeaderProps) {
  const status = compileStatusView(compileStatus);
  const Icon = status.Icon;

  return (
    <header className="header">
      <span className="header__logo">
        <AreaChart
          className="header__logo-mark"
          size={14}
          strokeWidth={2.4}
          aria-hidden="true"
        />
        Tellur
      </span>
      <span className="header__project">
        <span className="header__project-name" title={projectName}>
          {projectName}
        </span>
        <span className="header__project-divider">—</span>
        <span className="header__project-url">{url}</span>
        <span
          className={`header__compile header__compile--${compileStatus}`}
          title={compileError ?? status.label}
        >
          <Icon
            className="header__compile-icon"
            size={13}
            strokeWidth={2.2}
            aria-hidden="true"
          />
          <span>{status.label}</span>
        </span>
      </span>
      <span className="header__spacer" />
    </header>
  );
}

function compileStatusView(status: CompileStatus) {
  switch (status) {
    case "compiling":
      return { label: "Compiling...", Icon: LoaderCircle };
    case "failed":
      return { label: "Failed", Icon: TriangleAlert };
    case "disconnected":
      return { label: "Disconnected", Icon: TriangleAlert };
    case "compiled":
      return { label: "Compiled", Icon: CircleCheck };
  }
}
