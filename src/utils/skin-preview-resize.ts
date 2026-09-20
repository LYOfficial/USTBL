// Keep the current renderer and its resources while the layout is moving.
// Only resize the drawing buffers after the layout has settled.
interface ResizableSkinViewer {
  readonly width: number;
  readonly height: number;
  readonly canvas: HTMLCanvasElement;
  setSize(width: number, height: number): void;
  render(): void;
}

export const createSkinPreviewResizer = (
  viewer: ResizableSkinViewer,
  settleMs = 160
) => {
  let timer: ReturnType<typeof setTimeout> | undefined;
  let disposed = false;

  const retainCanvas = () => {
    // Scale uniformly by height; the wrapper centers and clips horizontally.
    // This keeps the character's proportions and apparent size consistent
    // with the camera after setSize, without stretching the old frame.
    viewer.canvas.style.width = "auto";
    viewer.canvas.style.height = "100%";
  };

  const cancel = () => {
    clearTimeout(timer);
    timer = undefined;
  };

  retainCanvas();

  return {
    resize(width: number, height: number) {
      cancel();
      if (
        disposed ||
        !Number.isFinite(width) ||
        !Number.isFinite(height) ||
        width < 1 ||
        height < 1
      ) {
        return;
      }
      const nextWidth = Math.round(width);
      const nextHeight = Math.round(height);
      if (viewer.width === nextWidth && viewer.height === nextHeight) return;

      timer = setTimeout(() => {
        timer = undefined;
        if (disposed) return;
        viewer.setSize(nextWidth, nextHeight);
        // setSize writes inline CSS sizes. Restore presentation sizing, then
        // draw synchronously so the cleared buffer is never shown to users.
        retainCanvas();
        viewer.render();
      }, settleMs);
    },
    dispose() {
      disposed = true;
      cancel();
    },
  };
};
