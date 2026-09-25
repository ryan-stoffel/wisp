export interface Task {
  id: string;
  title: string;
  done: boolean;
}

export function summarize(tasks: readonly Task[]): string {
  const done = tasks.filter((task) => task.done).length;
  return `${done} of ${tasks.length} tasks done`;
}
