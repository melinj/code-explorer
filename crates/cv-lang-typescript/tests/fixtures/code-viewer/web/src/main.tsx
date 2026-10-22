import { createRoot } from "react-dom/client";
import { App } from "./panels/App";
import { installAuth } from "./data/auth";
import "./panels/app.css";

installAuth();

createRoot(document.getElementById("root")!).render(<App />);
