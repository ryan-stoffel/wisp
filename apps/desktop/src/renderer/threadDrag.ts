// A sidebar thread row dragged onto the composer, which attaches it to the message (PLX-378).

/** The drag's data type. A thread's id is its host's, so the drag names the host too. */
export const threadDragType = "application/x-parallax-thread";

/** Puts thread `runId` on host `hostId` in a drag, for `draggedThread` to read on drop. */
export function dragThread(data: DataTransfer, hostId: string, runId: string) {
  data.setData(threadDragType, JSON.stringify({ hostId, runId }));
  data.effectAllowed = "copy";
}

/** The thread a drop carries, or undefined when it carries none. */
export function draggedThread(data: DataTransfer): { hostId: string; runId: string } | undefined {
  try {
    const { hostId, runId } = JSON.parse(data.getData(threadDragType)) as Record<string, unknown>;
    return typeof hostId === "string" && typeof runId === "string" ? { hostId, runId } : undefined;
  } catch {
    return undefined;
  }
}
