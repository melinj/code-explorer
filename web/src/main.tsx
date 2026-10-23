import { useEffect, useState } from "react";
import { createRoot } from "react-dom/client";
import type { SetupInfo } from "./contracts/http";
import { App } from "./panels/App";
import { GitRepoScreen, loadSetup } from "./panels/GitRepo";
import { installAuth } from "./data/auth";
import "./panels/app.css";

installAuth();

/** The full app on a jj repository; on a plain git repository (GET /setup), the git screen. */
function Root() {
  const [setup, setSetup] = useState<SetupInfo | null>();
  useEffect(() => {
    loadSetup().then((s) => setSetup(s ?? null), () => setSetup(null));
  }, []);
  if (setup === undefined) return null;
  return setup?.vcs === "git" ? <GitRepoScreen setup={setup} /> : <App />;
}

createRoot(document.getElementById("root")!).render(<Root />);
