import { ApiLogDashboard } from "./ApiLogDashboard";
import { currentView } from "../state/app";

export default function Dashboard() {
  return (
    <div class="view-container">
      <ApiLogDashboard active={currentView() === "dashboard"} />
    </div>
  );
}
