import { AreaChart } from "lucide-react";

interface HeaderProps {
  projectName: string;
  url: string;
}

export function Header({ projectName, url }: HeaderProps) {
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
        <span className="header__project-name">{projectName}</span>
        <span className="header__project-divider">—</span>
        <span className="header__project-url">{url}</span>
      </span>
      <span className="header__spacer" />
    </header>
  );
}
