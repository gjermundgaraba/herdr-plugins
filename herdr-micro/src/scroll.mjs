export function scrollPlan(pane, layout, direction, percent) {
  const rows = Number(pane.scroll?.viewport_rows);
  const rect = layout.panes?.find(({ pane_id }) => pane_id === pane.pane_id)?.rect;
  if (!pane.pane_id || !Number.isInteger(rows) || rows < 1 || !rect) {
    throw new Error("focused pane dimensions unavailable");
  }

  const width = layout.area.x + layout.area.width;
  const height = layout.area.y + layout.area.height;
  const notches = Math.max(1, Math.round((rows * percent) / 300));
  return {
    paneId: pane.pane_id,
    notches: direction === "up" ? notches : -notches,
    x: (rect.x + rect.width / 2) / width,
    y: (rect.y + rect.height / 2) / height,
  };
}
