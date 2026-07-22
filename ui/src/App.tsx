import { useEffect, useState } from "react";
import { getVersion } from "@tauri-apps/api/app";
import { getBinding, getSetting, setSetting, onDictationError, onMeetingError, onRewriteError, onRewriteProgress, onTtsError, onTtsState } from "./api";
import Onboarding from "./components/Onboarding";
import SpeechOverlay from "./components/SpeechOverlay";
import Spinner from "./components/Spinner";
import StatusPill from "./components/StatusPill";
import AiProvidersPage from "./pages/AiProvidersPage";
import DictationPage from "./pages/DictationPage";
import GeneralPage from "./pages/GeneralPage";
import HistoryPage from "./pages/HistoryPage";
import LogsPage from "./pages/LogsPage";
import MeetingsPage from "./pages/MeetingsPage";
import ModelsPage from "./pages/ModelsPage";
import ReadAloudPage from "./pages/ReadAloudPage";
import RewritePage from "./pages/RewritePage";
import { ThemeProvider, useTheme } from "./theme";

type Page =
  | "rewrite"
  | "dictation"
  | "meetings"
  | "read-aloud"
  | "ai-providers"
  | "models"
  | "general"
  | "history"
  | "logs";

const MOBILE_BREAKPOINT = 768;

function useIsMobile() {
  const [isMobile, setIsMobile] = useState(
    () => typeof window !== "undefined" && window.innerWidth <= MOBILE_BREAKPOINT,
  );

  useEffect(() => {
    const mq = window.matchMedia(`(max-width: ${MOBILE_BREAKPOINT}px)`);
    const onChange = () => setIsMobile(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, []);

  return isMobile;
}

function AppShell() {
  const [page, setPage] = useState<Page>("rewrite");
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);
  const [statusVariant, setStatusVariant] = useState<"progress" | "error" | null>(null);
  const { theme, toggle } = useTheme();
  const isMobile = useIsMobile();

  const [onboarding, setOnboarding] = useState<"loading" | "show" | "hide">("loading");

  const [version, setVersion] = useState<string | null>(null);

  useEffect(() => {
    getVersion()
      .then(setVersion)
      .catch(() => setVersion(null));
  }, []);

  useEffect(() => {
    let clearTimer: ReturnType<typeof setTimeout> | undefined;
    const showStatus = (message: string, variant: "progress" | "error") => {
      setStatusMessage(message);
      setStatusVariant(variant);
      clearTimeout(clearTimer);
      if (variant === "progress" && message.toLowerCase() === "done") {
        clearTimer = setTimeout(() => {
          setStatusMessage(null);
          setStatusVariant(null);
        }, 3000);
      }
    };

    const unsubs = Promise.all([
      onRewriteProgress((message) => showStatus(message, "progress")),
      onRewriteError((message) => showStatus(message, "error")),
      onDictationError((message) => showStatus(message, "error")),
      onTtsState((state) => {
        if (state === "reading") {
          showStatus("Reading selection aloud…", "progress");
        } else if (state === "idle") {
          showStatus("Done", "progress");
        }
      }),
      onTtsError((message) => showStatus(message, "error")),
      onMeetingError((message) => showStatus(message, "error")),
    ]);

    return () => {
      clearTimeout(clearTimer);
      void unsubs.then((fns) => fns.forEach((fn) => fn()));
    };
  }, []);

  useEffect(() => {
    if (!isMobile) setDrawerOpen(false);
  }, [isMobile]);

  const navigate = (next: Page) => {
    setPage(next);
    if (isMobile) setDrawerOpen(false);
  };

  const completeOnboarding = async () => {
    await setSetting("onboarding.completed", "true");
    setOnboarding("hide");
  };

  const runSetup = async () => {
    await setSetting("onboarding.completed", "false");
    setOnboarding("show");
  };

  useEffect(() => {
    let cancelled = false;
    const check = async () => {
      try {
        const completed = await getSetting("onboarding.completed");
        if (completed === "true") {
          if (!cancelled) setOnboarding("hide");
          return;
        }
        const binding = await getBinding("rewrite", "llm");
        if (binding) {
          await setSetting("onboarding.completed", "true");
          if (!cancelled) setOnboarding("hide");
          return;
        }
        if (!cancelled) setOnboarding("show");
      } catch {
        if (!cancelled) setOnboarding("hide");
      }
    };
    void check();
    return () => { cancelled = true; };
  }, []);

  const navItem = (id: Page, label: string, icon: string) => (
    <button
      key={id}
      type="button"
      className={`kea-nav-item${page === id ? " kea-nav-item--active" : ""}`}
      aria-current={page === id ? "page" : undefined}
      onClick={() => navigate(id)}
    >
      <span className="kea-nav-item__icon" aria-hidden="true">
        {icon}
      </span>
      {label}
    </button>
  );

  const renderPage = () => {
    switch (page) {
      case "rewrite":
        return <RewritePage onRunSetup={runSetup} />;
      case "dictation":
        return <DictationPage />;
      case "meetings":
        return <MeetingsPage />;
      case "read-aloud":
        return <ReadAloudPage />;
      case "ai-providers":
        return <AiProvidersPage />;
      case "models":
        return <ModelsPage />;
      case "general":
        return <GeneralPage onRunSetup={runSetup} />;
      case "history":
        return <HistoryPage />;
      case "logs":
        return <LogsPage />;
    }
  };

  return (
    <div className="kea-app">
      {onboarding === "loading" ? (
        <div
          style={{
            display: "flex",
            alignItems: "center",
            justifyContent: "center",
            height: "100vh",
            gap: 8,
          }}
        >
          <Spinner size={20} />
          <span className="kea-muted">Loading…</span>
        </div>
      ) : onboarding === "show" ? (
        <div className="kea-layout">
          <main className="kea-main">
            <Onboarding onFinish={completeOnboarding} />
          </main>
        </div>
      ) : (
        <>
      {isMobile && (
        <header className="kea-topbar">
          <button
            type="button"
            className="kea-icon-btn"
            aria-label="Open menu"
            aria-expanded={drawerOpen}
            aria-controls="kea-nav"
            onClick={() => setDrawerOpen((o) => !o)}
          >
            ☰
          </button>
          <h1 className="kea-topbar__title">KEA</h1>
        </header>
      )}

      <div className="kea-layout">
        {isMobile && (
          <div
            className={`kea-drawer-backdrop${drawerOpen ? " kea-drawer-backdrop--open" : ""}`}
            onClick={() => setDrawerOpen(false)}
            aria-hidden="true"
          />
        )}

        <nav
          id="kea-nav"
          className={`kea-drawer${isMobile && drawerOpen ? " kea-drawer--open" : ""}`}
          aria-label="Main navigation"
        >
          <div className="kea-drawer__group-label">Features</div>
          {navItem("rewrite", "Rewrite", "✏️")}
          {navItem("dictation", "Dictation", "🎙")}
          {navItem("meetings", "Meetings", "👥")}
          {navItem("read-aloud", "Read-aloud", "🔊")}

          <div className="kea-drawer__group-label">Activity</div>
          {navItem("history", "History", "🕘")}

          <div className="kea-drawer__group-label">Settings</div>
          {navItem("ai-providers", "AI Providers", "🤖")}
          {navItem("models", "Models", "📦")}
          {navItem("general", "General", "⚙️")}
          {navItem("logs", "Logs", "🛠")}

          <div className="kea-sidebar__footer">
            <button
              type="button"
              className="kea-icon-btn"
              aria-label={theme === "dark" ? "Switch to light mode" : "Switch to dark mode"}
              onClick={toggle}
            >
              {theme === "dark" ? "☀️" : "🌙"}
            </button>
            <span className="kea-muted">{version ? `v${version}` : ""}</span>
          </div>
        </nav>

        <main className="kea-main">{renderPage()}</main>
      </div>

      <SpeechOverlay />
      <StatusPill message={statusMessage} variant={statusVariant} />
        </>
      )}
    </div>
  );
}

export default function App() {
  return (
    <ThemeProvider>
      <AppShell />
    </ThemeProvider>
  );
}
