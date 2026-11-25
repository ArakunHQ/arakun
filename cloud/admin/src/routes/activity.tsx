import { createFileRoute } from "@tanstack/react-router";
import { ActivityList } from "../components/ActivityList";

export const Route = createFileRoute("/activity")({
  component: Activity,
});

function Activity() {
  return (
    <>
      <h1>Activity</h1>
      <ActivityList />
    </>
  );
}
