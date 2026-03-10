declare module 'jmuxer' {
  interface JMuxerOptions {
    node: string;
    mode?: 'video' | 'audio' | 'both';
    fps?: number;
    flushingTime?: number;
    debug?: boolean;
    onReady?: () => void;
    onError?: (error: unknown) => void;
  }

  interface FeedData {
    video?: Uint8Array;
    audio?: Uint8Array;
    duration?: number;
  }

  class JMuxer {
    constructor(options: JMuxerOptions);
    feed(data: FeedData): void;
    destroy(): void;
  }

  export default JMuxer;
}
