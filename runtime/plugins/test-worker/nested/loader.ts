export async function loadAnswer(): Promise<number> {
  return (await import("./helper.ts")).answer;
}
