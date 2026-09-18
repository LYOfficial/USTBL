import * as skinview3d from "skinview3d";

export class SpringAnimation extends skinview3d.PlayerAnimation {
  private readonly duration = 0.9;
  private readonly onComplete?: () => void;
  private completed = false;
  private baseScale?: { x: number; y: number; z: number };
  private baseY?: number;

  constructor(onComplete?: () => void) {
    super();
    this.onComplete = onComplete;
  }

  protected animate(player: skinview3d.PlayerObject): void {
    this.baseScale ??= {
      x: player.scale.x,
      y: player.scale.y,
      z: player.scale.z,
    };
    this.baseY ??= player.position.y;

    const progress = Math.min(this.progress / this.duration, 1);
    const damping = 1 - progress;
    const oscillation = Math.sin(progress * Math.PI * 3.5) * damping;

    player.position.y = this.baseY + oscillation * 0.18;
    player.scale.x = this.baseScale.x * (1 - oscillation * 0.07);
    player.scale.y = this.baseScale.y * (1 + oscillation * 0.14);
    player.scale.z = this.baseScale.z * (1 - oscillation * 0.07);
    player.rotation.z = oscillation * 0.04;

    if (progress >= 1 && !this.completed) {
      this.completed = true;
      player.position.y = this.baseY;
      player.scale.set(this.baseScale.x, this.baseScale.y, this.baseScale.z);
      player.rotation.z = 0;
      this.onComplete?.();
    }
  }
}
