import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type MutableRefObject,
} from "react";
import { getVersion } from "@tauri-apps/api/app";
import { getBinding, getSetting, setSetting, onDictationError, onMeetingError, onRewriteError, onRewriteProgress, onTtsError, onTtsState } from "./api";
import Onboarding from "./components/Onboarding";
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

/**
 * Everything focusable a drawer can hold. Enough to trap Tab without pulling
 * in a focus-trap dependency.
 */
const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/**
 * One source of truth: the media query. Seeding from window.innerWidth as well
 * meant the two could disagree (they round differently, and innerWidth counts
 * the scrollbar) until the first change event arrived.
 */
function useIsMobile() {
  const [mq] = useState<MediaQueryList | null>(() =>
    typeof window !== "undefined" && typeof window.matchMedia === "function"
      ? window.matchMedia(`(max-width: ${MOBILE_BREAKPOINT}px)`)
      : null,
  );
  const [isMobile, setIsMobile] = useState(() => mq?.matches ?? false);

  useEffect(() => {
    if (!mq) return;
    // Re-sync first: the viewport can change between the seeding render and
    // this effect, and that change fires no event we are subscribed to yet.
    setIsMobile(mq.matches);
    const onChange = () => setIsMobile(mq.matches);
    mq.addEventListener("change", onChange);
    return () => mq.removeEventListener("change", onChange);
  }, [mq]);

  return isMobile;
}

/**
 * Applies the `inert` attribute to a node. React 18 has no `inert` prop, and an
 * effect would miss the first mount of anything that appears later than the
 * shell — the drawer only mounts once the onboarding check resolves — so this
 * runs as a callback ref instead.
 */
function useInertRef<T extends HTMLElement>(
  inert: boolean,
  store?: MutableRefObject<T | null>,
) {
  return useCallback(
    (node: T | null) => {
      if (store) store.current = node;
      if (!node) return;
      if (inert) node.setAttribute("inert", "");
      else node.removeAttribute("inert");
    },
    [inert, store],
  );
}

function AppShell() {
  const [page, setPage] = useState<Page>("rewrite");
  const [drawerOpen, setDrawerOpen] = useState(false);
  const [statusMessage, setStatusMessage] = useState<string | null>(null);
  const [statusVariant, setStatusVariant] = useState<"progress" | "error" | null>(null);
  const { theme, toggle } = useTheme();
  const isMobile = useIsMobile();
  const drawerRef = useRef<HTMLDivElement | null>(null);
  const menuButtonRef = useRef<HTMLButtonElement>(null);

  const closeDrawer = useCallback(() => {
    // Reclaim focus only if the drawer actually held it. navigate() also
    // closes the drawer, and on mobile it runs for in-page navigation too —
    // a feature banner's "Open AI Providers", say, with the drawer never
    // open. Focusing the hamburger there would yank the user to the topbar.
    const hadFocus = drawerRef.current?.contains(document.activeElement) ?? false;
    setDrawerOpen(false);
    if (hadFocus) menuButtonRef.current?.focus();
  }, []);

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

  // A drawer that is off-screen must not be tabbable, and an open one must not
  // leave the page behind it reachable — Tab is trapped, but a virtual cursor
  // or a pointer would still get there. `visibility: hidden` covers the closed
  // drawer in CSS; `inert` states both for assistive tech and is honoured by
  // every engine KEA ships on (WebKit 15.5+, Chromium 102+, Gecko 112+).
  const attachDrawer = useInertRef(isMobile && !drawerOpen, drawerRef);
  const attachMain = useInertRef(isMobile && drawerOpen);

  // While the drawer is open it owns the keyboard: Escape closes it and Tab
  // cycles inside it instead of wandering into the page behind.
  useEffect(() => {
    if (!isMobile || !drawerOpen) return;
    const el = drawerRef.current;
    el?.querySelector<HTMLElement>(FOCUSABLE)?.focus();

    const onKeyDown = (event: KeyboardEvent) => {
      if (event.key === "Escape") {
        event.preventDefault();
        closeDrawer();
        return;
      }
      if (event.key !== "Tab" || !el) return;
      const items = Array.from(el.querySelectorAll<HTMLElement>(FOCUSABLE));
      if (items.length === 0) return;
      const first = items[0];
      const last = items[items.length - 1];
      const active = document.activeElement;
      const outside = !el.contains(active);
      if (event.shiftKey && (active === first || outside)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && (active === last || outside)) {
        event.preventDefault();
        first.focus();
      }
    };

    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [isMobile, drawerOpen, closeDrawer]);

  const navigate = (next: Page) => {
    setPage(next);
    if (isMobile) closeDrawer();
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
        return <RewritePage onRunSetup={runSetup} onNavigate={navigate} />;
      case "dictation":
        return <DictationPage onNavigate={navigate} />;
      case "meetings":
        return <MeetingsPage onNavigate={navigate} />;
      case "read-aloud":
        return <ReadAloudPage onNavigate={navigate} />;
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
            ref={menuButtonRef}
            type="button"
            className="kea-icon-btn"
            aria-label="Open menu"
            aria-expanded={drawerOpen}
            aria-controls="kea-nav"
            onClick={() => (drawerOpen ? closeDrawer() : setDrawerOpen(true))}
          >
            ☰
          </button>
          {/* Brand label, not the document heading: every page now owns its
              own h1, so a second one here would give the mobile outline two. */}
          <span className="kea-topbar__title">KEA</span>
        </header>
      )}

      <div className="kea-layout">
        {isMobile && (
          <div
            className={`kea-drawer-backdrop${drawerOpen ? " kea-drawer-backdrop--open" : ""}`}
            onClick={closeDrawer}
            aria-hidden="true"
          />
        )}

        {/* The sidebar column, not the nav landmark: the theme toggle and the
            version below are app chrome, so they sit outside <nav>. */}
        <div
          id="kea-nav"
          ref={attachDrawer}
          className={`kea-drawer${isMobile && drawerOpen ? " kea-drawer--open" : ""}`}
        >
          <nav className="kea-drawer__nav" aria-label="Main navigation">
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
          </nav>

          {/* A labelled section, so the toggle and version sit in a landmark
              of their own instead of being orphaned outside every one. */}
          <section className="kea-sidebar__footer" aria-label="Theme and version">
            <button
              type="button"
              className="kea-icon-btn"
              aria-label={theme === "dark" ? "Switch to light mode" : "Switch to dark mode"}
              onClick={toggle}
            >
              {theme === "dark" ? "☀️" : "🌙"}
            </button>
            <span className="kea-muted">{version ? `v${version}` : ""}</span>
          </section>
        </div>

        <main className="kea-main" ref={attachMain}>
          {renderPage()}
        </main>
      </div>

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
