import { checkSurface } from "./surface.js";

export function checkBaselineSurface(): number {
  return checkSurface((body) => body);
}
