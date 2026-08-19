// ── 0. AOT Template ───────────────────────────────────────────────────────

// Data statique globale : injectée une seule fois.
const _SVG_SPRITES = `
<svg xmlns="http://www.w3.org/2000/svg" id="media-sprites" hidden aria-hidden="true">
  <symbol id="media-fast-forward" viewBox="0 0 24 24"><path d="M5.58 16.89l5.77-4.07c.56-.4.56-1.24 0-1.63L5.58 7.11C4.91 6.65 4 7.12 4 7.93v8.14c0 .81.91 1.28 1.58.82zM13 7.93v8.14c0 .81.91 1.28 1.58.82l5.77-4.07c.56-.4.56-1.24 0-1.63l-5.77-4.07c-.67-.47-1.58 0-1.58.81z"></path></symbol>
  <symbol id="media-fast-rewind" viewBox="0 0 24 24"><path d="M11 16.07V7.93c0-.81-.91-1.28-1.58-.82l-5.77 4.07c-.56.4-.56 1.24 0 1.63l5.77 4.07c.67.47 1.58 0 1.58-.81zm1.66-3.25l5.77 4.07c.66.47 1.58-.01 1.58-.82V7.93c0-.81-.91-1.28-1.58-.82l-5.77 4.07c-.57.4-.57 1.24 0 1.64z"></path></symbol>
  <symbol id="media-favorite-border" viewBox="0 0 24 24"><path d="M0 0h24v24H0V0z" fill="none"></path><path d="M19.66 3.99c-2.64-1.8-5.9-.96-7.66 1.1-1.76-2.06-5.02-2.91-7.66-1.1-1.4.96-2.28 2.58-2.34 4.29-.14 3.88 3.3 6.99 8.55 11.76l.1.09c.76.69 1.93.69 2.69-.01l.11-.1c5.25-4.76 8.68-7.87 8.55-11.75-.06-1.7-.94-3.32-2.34-4.28zM12.1 18.55l-.1.1-.1-.1C7.14 14.24 4 11.39 4 8.5 4 6.5 5.5 5 7.5 5c1.54 0 3.04.99 3.57 2.36h1.87C13.46 5.99 14.96 5 16.5 5c2 0 3.5 1.5 3.5 3.5 0 2.89-3.14 5.74-7.9 10.05z"></path></symbol>
  <symbol id="media-favorite" viewBox="0 0 24 24"><path d="M0 0h24v24H0V0z" fill="none"></path><path d="M13.35 20.13c-.76.69-1.93.69-2.69-.01l-.11-.1C5.3 15.27 1.87 12.16 2 8.28c.06-1.7.93-3.33 2.34-4.29 2.64-1.8 5.9-.96 7.66 1.1 1.76-2.06 5.02-2.91 7.66-1.1 1.41.96 2.28 2.59 2.34 4.29.14 3.88-3.3 6.99-8.55 11.76l-.1.09z"></path></symbol>
  <symbol id="media-forward-10" viewBox="0 0 24 24"><g><rect fill="none" height="24" width="24"></rect><rect fill="none" height="24" width="24"></rect><rect fill="none" height="24" width="24"></rect></g><g><g><path d="M18,13c0,3.31-2.69,6-6,6s-6-2.69-6-6s2.69-6,6-6v4l5-5l-5-5v4c-4.42,0-8,3.58-8,8c0,4.42,3.58,8,8,8s8-3.58,8-8H18z"></path><polygon points="10.9,16 10.9,11.73 10.81,11.73 9.04,12.36 9.04,13.05 10.05,12.74 10.05,16"></polygon><path d="M14.32,11.78c-0.18-0.07-0.37-0.1-0.59-0.1s-0.41,0.03-0.59,0.1s-0.33,0.18-0.45,0.33s-0.23,0.34-0.29,0.57 s-0.1,0.5-0.1,0.82v0.74c0,0.32,0.04,0.6,0.11,0.82s0.17,0.42,0.3,0.57s0.28,0.26,0.46,0.33s0.37,0.1,0.59,0.1s0.41-0.03,0.59-0.1 s0.33-0.18,0.45-0.33s0.22-0.34,0.29-0.57s0.1-0.5,0.1-0.82V13.5c0-0.32-0.04-0.6-0.11-0.82s-0.17-0.42-0.3-0.57 S14.49,11.85,14.32,11.78z M14.33,14.35c0,0.19-0.01,0.35-0.04,0.48s-0.06,0.24-0.11,0.32s-0.11,0.14-0.19,0.17 s-0.16,0.05-0.25,0.05s-0.18-0.02-0.25-0.05s-0.14-0.09-0.19-0.17s-0.09-0.19-0.12-0.32s-0.04-0.29-0.04-0.48v-0.97 c0-0.19,0.01-0.35,0.04-0.48s0.06-0.23,0.12-0.31s0.11-0.14,0.19-0.17s0.16-0.05,0.25-0.05s0.18,0.02,0.25,0.05 s0.14,0.09,0.19,0.17s0.09,0.18,0.12,0.31s0.04,0.29,0.04,0.48V14.35z"></path></g></g></symbol>
  <symbol id="media-forward-30" viewBox="0 0 24 24"><path d="M18.92 13c-.5 0-.91.37-.98.86-.48 3.37-3.77 5.84-7.42 4.96-2.25-.54-3.91-2.27-4.39-4.53C5.32 10.42 8.27 7 12 7v2.79c0 .45.54.67.85.35l3.79-3.79c.2-.2.2-.51 0-.71l-3.79-3.79c-.31-.31-.85-.09-.85.36V5c-4.94 0-8.84 4.48-7.84 9.6.6 3.11 2.9 5.5 5.99 6.19 4.83 1.08 9.15-2.2 9.77-6.67.09-.59-.4-1.12-1-1.12zm-8.38 2.22c-.06.05-.12.09-.2.12s-.17.04-.27.04c-.09 0-.17-.01-.25-.04s-.14-.06-.2-.11-.1-.1-.13-.17-.05-.14-.05-.22h-.85c0 .21.04.39.12.55s.19.28.33.38.29.18.46.23.35.07.53.07c.21 0 .41-.03.6-.08s.34-.14.48-.24.24-.24.32-.39.12-.33.12-.53c0-.23-.06-.44-.18-.61s-.3-.3-.54-.39c.1-.05.2-.1.28-.17s.15-.14.2-.22.1-.16.13-.25.04-.18.04-.27c0-.2-.04-.37-.11-.53s-.17-.28-.3-.38-.28-.18-.46-.23-.37-.08-.59-.08c-.19 0-.38.03-.54.08s-.32.13-.44.23-.23.22-.3.37-.11.3-.11.48h.85c0-.07.02-.14.05-.2s.07-.11.12-.15.11-.07.18-.1.14-.03.22-.03c.1 0 .18.01.25.04s.13.06.18.11.08.11.11.17.04.14.04.22c0 .18-.05.32-.16.43s-.26.16-.48.16h-.43v.66h.45c.11 0 .2.01.29.04s.16.06.22.11.11.12.14.2.05.18.05.29c0 .09-.01.17-.04.24s-.08.11-.13.17zm3.9-3.44c-.18-.07-.37-.1-.59-.1s-.41.03-.59.1-.33.18-.45.33-.23.34-.29.57-.1.5-.1.82v.74c0 .32.04.6.11.82s.17.42.3.57.28.26.46.33.37.1.59.1.41-.03.59-.1.33-.18.45-.33.22-.34.29-.57.1-.5.1-.82v-.74c0-.32-.04-.6-.11-.82s-.17-.42-.3-.57-.28-.26-.46-.33zm.01 2.57c0 .19-.01.35-.04.48s-.06.24-.11.32-.11.14-.19.17-.16.05-.25.05-.18-.02-.25-.05-.14-.09-.19-.17-.09-.19-.12-.32-.04-.29-.04-.48v-.97c0-.19.01-.35.04-.48s.06-.23.12-.31.11-.14.19-.17.16-.05.25-.05.18.02.25.05.14.09.19.17.09.18.12.31.04.29.04.48v.97z"></path></symbol>
  <symbol id="media-forward-5" viewBox="0 0 24 24"><path d="M18.87 13c-.5 0-.91.37-.98.86-.48 3.37-3.77 5.84-7.42 4.96-2.25-.54-3.91-2.27-4.39-4.53C5.27 10.42 8.22 7 11.95 7v2.79c0 .45.54.67.85.35l3.79-3.79c.2-.2.2-.51 0-.71L12.8 1.85c-.31-.31-.85-.09-.85.35V5c-4.94 0-8.84 4.48-7.84 9.6.6 3.11 2.9 5.5 5.99 6.19 4.83 1.08 9.15-2.2 9.77-6.67.09-.59-.4-1.12-1-1.12zm-6.44 2.15c-.05.07-.11.13-.18.17s-.17.06-.27.06c-.17 0-.31-.05-.42-.15s-.17-.24-.19-.41h-.84c.01.2.05.37.13.53s.19.28.32.39.29.19.46.24.35.08.53.08c.24 0 .46-.04.64-.12s.33-.18.45-.31.21-.28.27-.45.09-.35.09-.54c0-.22-.03-.43-.09-.6s-.14-.33-.25-.45-.25-.22-.41-.28-.34-.1-.55-.1c-.07 0-.14.01-.2.02s-.13.02-.18.04-.1.03-.15.05-.08.04-.11.05l.11-.92h1.7v-.71H10.9l-.25 2.17.67.17c.03-.03.06-.06.1-.09s.07-.05.12-.07.1-.04.15-.05.13-.02.2-.02c.12 0 .22.02.3.05s.16.09.21.15.1.14.13.24.04.19.04.31-.01.22-.03.31-.06.17-.11.24z"></path></symbol>
  <symbol id="media-fullscreen" viewBox="0 0 24 24"><path d="M0 0h24v24H0V0z" fill="none"></path><path d="M6 14c-.55 0-1 .45-1 1v3c0 .55.45 1 1 1h3c.55 0 1-.45 1-1s-.45-1-1-1H7v-2c0-.55-.45-1-1-1zm0-4c.55 0 1-.45 1-1V7h2c.55 0 1-.45 1-1s-.45-1-1-1H6c-.55 0-1 .45-1 1v3c0 .55.45 1 1 1zm11 7h-2c-.55 0-1 .45-1 1s.45 1 1 1h3c.55 0 1-.45 1-1v-3c0-.55-.45-1-1-1s-1 .45-1 1v2zM14 6c0 .55.45 1 1 1h2v2c0 .55.45 1 1 1s1-.45 1-1V6c0-.55-.45-1-1-1h-3c-.55 0-1 .45-1 1z"></path></symbol>
  <symbol id="media-get-app" viewBox="0 0 24 24"><path d="M0 0h24v24H0V0z" fill="none"></path><path d="M16.59 9H15V4c0-.55-.45-1-1-1h-4c-.55 0-1 .45-1 1v5H7.41c-.89 0-1.34 1.08-.71 1.71l4.59 4.59c.39.39 1.02.39 1.41 0l4.59-4.59c.63-.63.19-1.71-.7-1.71zM5 19c0 .55.45 1 1 1h12c.55 0 1-.45 1-1s-.45-1-1-1H6c-.55 0-1 .45-1 1z"></path></symbol>
  <symbol id="media-menu" viewBox="0 0 24 24"><path d="M0 0h24v24H0V0z" fill="none"></path><path d="M12 8c1.1 0 2-.9 2-2s-.9-2-2-2-2 .9-2 2 .9 2 2 2zm0 2c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2zm0 6c-1.1 0-2 .9-2 2s.9 2 2 2 2-.9 2-2-.9-2-2-2z"></path></symbol>
  <symbol id="media-move-down" viewBox="0 0 24 24"><g><rect fill="none" height="24" width="24"></rect><rect fill="none" height="24" width="24"></rect></g><g><g><path d="M3.01,10.72c-0.14,2.57,1.66,4.73,4.07,5.18l-0.79-0.79c-0.39-0.39-0.39-1.02,0-1.41l0,0c0.39-0.39,1.02-0.39,1.41,0 l2.59,2.59c0.39,0.39,0.39,1.02,0,1.41L7.71,20.3c-0.39,0.39-1.02,0.39-1.41,0h0c-0.39-0.39-0.39-1.02,0-1.41l0.88-0.88l0-0.06 c-3.64-0.43-6.43-3.65-6.15-7.47C1.29,6.78,4.55,4,8.26,4L10,4c0.55,0,1,0.45,1,1v0c0,0.55-0.45,1-1,1L8.22,6 C5.52,6,3.15,8.04,3.01,10.72z"></path><path d="M15,11h5c1.1,0,2-0.9,2-2V6c0-1.1-0.9-2-2-2h-5c-1.1,0-2,0.9-2,2v3C13,10.1,13.9,11,15,11z M20,9h-5V6h5V9z"></path><path d="M20,20h-5c-1.1,0-2-0.9-2-2v-3c0-1.1,0.9-2,2-2h5c1.1,0,2,0.9,2,2v3C22,19.1,21.1,20,20,20z"></path></g></g></symbol>
  <symbol id="media-move-up" viewBox="0 0 24 24"><g><rect fill="none" height="24" width="24"></rect><rect fill="none" height="24" width="24"></rect></g><g><g><path d="M3.01,13.28c-0.14-2.57,1.66-4.73,4.07-5.18L6.29,8.88c-0.39,0.39-0.39,1.02,0,1.41l0,0c0.39,0.39,1.02,0.39,1.41,0 l2.59-2.59c0.39-0.39,0.39-1.02,0-1.41L7.71,3.7c-0.39-0.39-1.02-0.39-1.41,0l0,0c-0.39,0.39-0.39,1.02,0,1.41l0.88,0.88l0,0.06 c-3.64,0.43-6.43,3.65-6.15,7.47C1.29,17.22,4.55,20,8.26,20H10c0.55,0,1-0.45,1-1v0c0-0.55-0.45-1-1-1H8.22 C5.52,18,3.15,15.96,3.01,13.28z"></path><path d="M13,15v3c0,1.1,0.9,2,2,2h5c1.1,0,2-0.9,2-2v-3c0-1.1-0.9-2-2-2h-5C13.9,13,13,13.9,13,15z M20,18h-5v-3h5V18z"></path><path d="M20,4h-5c-1.1,0-2,0.9-2,2v3c0,1.1,0.9,2,2,2h5c1.1,0,2-0.9,2-2V6C22,4.9,21.1,4,20,4z"></path></g></g></symbol>
  <symbol id="media-pause" viewBox="0 0 24 24"><path d="M8 19c1.1 0 2-.9 2-2V7c0-1.1-.9-2-2-2s-2 .9-2 2v10c0 1.1.9 2 2 2zm6-12v10c0 1.1.9 2 2 2s2-.9 2-2V7c0-1.1-.9-2-2-2s-2 .9-2 2z"></path></symbol>
  <symbol id="media-picture-in-picture-alt" viewBox="0 0 24 24"><path d="M0 0h24v24H0V0z" fill="none"></path><path d="M18 11h-6c-.55 0-1 .45-1 1v4c0 .55.45 1 1 1h6c.55 0 1-.45 1-1v-4c0-.55-.45-1-1-1zm5 8V4.98C23 3.88 22.1 3 21 3H3c-1.1 0-2 .88-2 1.98V19c0 1.1.9 2 2 2h18c1.1 0 2-.9 2-2zm-3 .02H4c-.55 0-1-.45-1-1V5.97c0-.55.45-1 1-1h16c.55 0 1 .45 1 1v12.05c0 .55-.45 1-1 1z"></path></symbol>
  <symbol id="media-picture-in-picture" viewBox="0 0 24 24"><path d="M0 0h24v24H0V0z" fill="none"></path><path d="M18 7h-6c-.55 0-1 .45-1 1v4c0 .55.45 1 1 1h6c.55 0 1-.45 1-1V8c0-.55-.45-1-1-1zm3-4H3c-1.1 0-2 .9-2 2v14c0 1.1.9 1.98 2 1.98h18c1.1 0 2-.88 2-1.98V5c0-1.1-.9-2-2-2zm-1 16.01H4c-.55 0-1-.45-1-1V5.98c0-.55.45-1 1-1h16c.55 0 1 .45 1 1v12.03c0 .55-.45 1-1 1z"></path></symbol>
  <symbol id="media-play-disabled" viewBox="0 0 24 24"><path d="M2.1,3.51L2.1,3.51c-0.39,0.39-0.39,1.02,0,1.41l5.9,5.9v6.35c0,0.79,0.87,1.27,1.54,0.84l3.45-2.2l6.08,6.08 c0.39,0.39,1.02,0.39,1.41,0l0,0c0.39-0.39,0.39-1.02,0-1.41L3.51,3.51C3.12,3.12,2.49,3.12,2.1,3.51z M17.68,12.84 c0.62-0.39,0.62-1.29,0-1.69L9.54,5.98C9.27,5.81,8.97,5.79,8.7,5.87l7.75,7.75L17.68,12.84z"></path></symbol>
  <symbol id="media-play" viewBox="0 0 24 24"><path d="M8 6.82v10.36c0 .79.87 1.27 1.54.84l8.14-5.18c.62-.39.62-1.29 0-1.69L9.54 5.98C8.87 5.55 8 6.03 8 6.82z"></path></symbol>
  <symbol id="media-playlist-play" viewBox="0 0 24 24"><path d="M5 10h10c.55 0 1 .45 1 1s-.45 1-1 1H5c-.55 0-1-.45-1-1s.45-1 1-1zm0-4h10c.55 0 1 .45 1 1s-.45 1-1 1H5c-.55 0-1-.45-1-1s.45-1 1-1zm0 8h6c.55 0 1 .45 1 1s-.45 1-1 1H5c-.55 0-1-.45-1-1s.45-1 1-1zm9 .88v4.23c0 .39.42.63.76.43l3.53-2.12c.32-.19.32-.66 0-.86l-3.53-2.12c-.34-.19-.76.05-.76.44z"></path></symbol>
  <symbol id="media-replay" viewBox="0 0 24 24"><path d="M12 5V2.21c0-.45-.54-.67-.85-.35l-3.8 3.79c-.2.2-.2.51 0 .71l3.79 3.79c.32.31.86.09.86-.36V7c3.73 0 6.68 3.42 5.86 7.29-.47 2.27-2.31 4.1-4.57 4.57-3.57.75-6.75-1.7-7.23-5.01-.07-.48-.49-.85-.98-.85-.6 0-1.08.53-1 1.13.62 4.39 4.8 7.64 9.53 6.72 3.12-.61 5.63-3.12 6.24-6.24C20.84 9.48 16.94 5 12 5z"></path></symbol>
  <symbol id="media-rewind-10" viewBox="0 0 24 24"><path d="M11.99 5V2.21c0-.45-.54-.67-.85-.35L7.35 5.65c-.2.2-.2.51 0 .71l3.79 3.79c.31.31.85.09.85-.35V7c3.73 0 6.68 3.42 5.86 7.29-.47 2.27-2.31 4.1-4.57 4.57-3.57.75-6.75-1.7-7.23-5.01-.06-.48-.48-.85-.98-.85-.6 0-1.08.53-1 1.13.62 4.39 4.8 7.64 9.53 6.72 3.12-.61 5.63-3.12 6.24-6.24.99-5.13-2.9-9.61-7.85-9.61zm-1.1 11h-.85v-3.26l-1.01.31v-.69l1.77-.63h.09V16zm4.28-1.76c0 .32-.03.6-.1.82s-.17.42-.29.57-.28.26-.45.33-.37.1-.59.1-.41-.03-.59-.1-.33-.18-.46-.33-.23-.34-.3-.57-.11-.5-.11-.82v-.74c0-.32.03-.6.1-.82s.17-.42.29-.57.28-.26.45-.33.37-.1.59-.1.41.03.59.1.33.18.46.33.23.34.3.57.11.5.11.82v.74zm-.85-.86c0-.19-.01-.35-.04-.48s-.07-.23-.12-.31-.11-.14-.19-.17-.16-.05-.25-.05-.18.02-.25.05-.14.09-.19.17-.09.18-.12.31-.04.29-.04.48v.97c0 .19.01.35.04.48s.07.24.12.32.11.14.19.17.16.05.25.05.18-.02.25-.05.14-.09.19-.17.09-.19.11-.32.04-.29.04-.48v-.97z"></path></symbol>
  <symbol id="media-rewind-30" viewBox="0 0 24 24"><path d="M12 5V2.21c0-.45-.54-.67-.85-.35l-3.8 3.79c-.2.2-.2.51 0 .71l3.79 3.79c.32.31.86.09.86-.36V7c3.73 0 6.68 3.42 5.86 7.29-.47 2.27-2.31 4.1-4.57 4.57-3.57.75-6.75-1.7-7.23-5.01-.07-.48-.49-.85-.98-.85-.6 0-1.08.53-1 1.13.62 4.39 4.8 7.64 9.53 6.72 3.12-.61 5.63-3.12 6.24-6.24C20.84 9.48 16.94 5 12 5zm-2.44 8.49h.45c.21 0 .37-.05.48-.16s.16-.25.16-.43c0-.08-.01-.15-.04-.22s-.06-.12-.11-.17-.11-.09-.18-.11-.16-.04-.25-.04c-.08 0-.15.01-.22.03s-.13.05-.18.1-.09.09-.12.15-.05.13-.05.2h-.85c0-.18.04-.34.11-.48s.17-.27.3-.37.27-.18.44-.23.35-.08.54-.08c.21 0 .41.03.59.08s.33.13.46.23.23.23.3.38.11.33.11.53c0 .09-.01.18-.04.27s-.07.17-.13.25-.12.15-.2.22-.17.12-.28.17c.24.09.42.21.54.39s.18.38.18.61c0 .2-.04.38-.12.53s-.18.29-.32.39-.29.19-.48.24-.38.08-.6.08c-.18 0-.36-.02-.53-.07s-.33-.12-.46-.23-.25-.23-.33-.38-.12-.34-.12-.55h.85c0 .08.02.15.05.22s.07.12.13.17.12.09.2.11.16.04.25.04c.1 0 .19-.01.27-.04s.15-.07.2-.12.1-.11.13-.18.04-.15.04-.24c0-.11-.02-.21-.05-.29s-.08-.15-.14-.2-.13-.09-.22-.11-.18-.04-.29-.04h-.47v-.65zm5.74.75c0 .32-.03.6-.1.82s-.17.42-.29.57-.28.26-.45.33-.37.1-.59.1-.41-.03-.59-.1-.33-.18-.46-.33-.23-.34-.3-.57-.11-.5-.11-.82v-.74c0-.32.03-.6.1-.82s.17-.42.29-.57.28-.26.45-.33.37-.1.59-.1.41.03.59.1.33.18.46.33.23.34.3.57.11.5.11.82v.74zm-.85-.86c0-.19-.01-.35-.04-.48s-.07-.23-.12-.31-.11-.14-.19-.17-.16-.05-.25-.05-.18.02-.25.05-.14.09-.19.17-.09.18-.12.31-.04.29-.04.48v.97c0 .19.01.35.04.48s.07.24.12.32.11.14.19.17.16.05.25.05.18-.02.25-.05.14-.09.19-.17.09-.19.11-.32c.03-.13.04-.29.04-.48v-.97z"></path></symbol>
  <symbol id="media-rewind-5" viewBox="0 0 24 24"><path d="M12 5V2.21c0-.45-.54-.67-.85-.35l-3.8 3.79c-.2.2-.2.51 0 .71l3.79 3.79c.32.31.86.09.86-.36V7c3.73 0 6.68 3.42 5.86 7.29-.47 2.26-2.14 3.99-4.39 4.53-3.64.88-6.93-1.6-7.42-4.96-.06-.49-.48-.86-.97-.86-.6 0-1.08.53-1 1.13.63 4.47 4.94 7.75 9.77 6.67 3.09-.69 5.39-3.08 5.99-6.19C20.84 9.48 16.94 5 12 5zm-1.31 8.9l.25-2.17h2.39v.71h-1.7l-.11.92c.03-.02.07-.03.11-.05s.09-.04.15-.05.12-.03.18-.04.13-.02.2-.02c.21 0 .39.03.55.1s.3.16.41.28.2.27.25.45.09.38.09.6c0 .19-.03.37-.09.54s-.15.32-.27.45-.27.24-.45.31-.39.12-.64.12c-.18 0-.36-.03-.53-.08s-.32-.14-.46-.24-.24-.24-.32-.39-.13-.33-.13-.53h.84c.02.18.08.32.19.41s.25.15.42.15c.11 0 .2-.02.27-.06s.14-.1.18-.17.08-.15.11-.25.03-.2.03-.31-.01-.21-.04-.31-.07-.17-.13-.24-.13-.12-.21-.15-.19-.05-.3-.05c-.08 0-.15.01-.2.02s-.11.03-.15.05-.08.05-.12.07-.07.06-.1.09l-.67-.16z"></path></symbol>
  <symbol id="media-skip-next" viewBox="0 0 24 24"><path d="M7.58 16.89l5.77-4.07c.56-.4.56-1.24 0-1.63L7.58 7.11C6.91 6.65 6 7.12 6 7.93v8.14c0 .81.91 1.28 1.58.82zM16 7v10c0 .55.45 1 1 1s1-.45 1-1V7c0-.55-.45-1-1-1s-1 .45-1 1z"></path></symbol>
  <symbol id="media-skip-prev" viewBox="0 0 24 24"><path d="M7 6c.55 0 1 .45 1 1v10c0 .55-.45 1-1 1s-1-.45-1-1V7c0-.55.45-1 1-1zm3.66 6.82l5.77 4.07c.66.47 1.58-.01 1.58-.82V7.93c0-.81-.91-1.28-1.58-.82l-5.77 4.07c-.57.4-.57 1.24 0 1.64z"></path></symbol>
  <symbol id="media-slow-motion" viewBox="0 0 24 24"><path d="M10 8.5v7c0 .41.47.65.8.4l4.67-3.5c.27-.2.27-.6 0-.8L10.8 8.1c-.33-.25-.8-.01-.8.4zm1-5.27c0-.64-.59-1.13-1.21-.99-1.12.26-2.18.7-3.12 1.3-.53.34-.61 1.1-.16 1.55.32.32.83.4 1.21.16.77-.49 1.62-.85 2.54-1.05.44-.1.74-.51.74-.97zM5.1 6.51c-.46-.45-1.21-.38-1.55.16-.6.94-1.04 2-1.3 3.12-.14.62.34 1.21.98 1.21.45 0 .87-.3.96-.74.2-.91.57-1.77 1.05-2.53.26-.39.18-.9-.14-1.22zM3.23 13c-.64 0-1.13.59-.99 1.21.26 1.12.7 2.17 1.3 3.12.34.54 1.1.61 1.55.16.32-.32.4-.83.15-1.21-.49-.76-.85-1.61-1.05-2.53-.09-.45-.5-.75-.96-.75zm3.44 7.45c.95.6 2 1.04 3.12 1.3.62.14 1.21-.35 1.21-.98 0-.45-.3-.87-.74-.96-.91-.2-1.77-.57-2.53-1.05-.39-.24-.89-.17-1.21.16-.46.44-.39 1.19.15 1.53zM22 12c0 4.73-3.3 8.71-7.73 9.74-.62.15-1.22-.34-1.22-.98 0-.46.31-.86.75-.97 3.55-.82 6.2-4 6.2-7.79s-2.65-6.97-6.2-7.79c-.44-.1-.75-.51-.75-.97 0-.64.6-1.13 1.22-.98C18.7 3.29 22 7.27 22 12z"></path></symbol>
  <symbol id="media-stop" viewBox="0 0 24 24"><path d="M8 6h8c1.1 0 2 .9 2 2v8c0 1.1-.9 2-2 2H8c-1.1 0-2-.9-2-2V8c0-1.1.9-2 2-2z"></path></symbol>
  <symbol id="media-subtitles-off" viewBox="0 0 24 24"><g><rect fill="none" height="24" width="24"></rect><rect fill="none" height="24" width="24"></rect></g><g><g><path d="M20,4H6.83l8,8H19c0.55,0,1,0.45,1,1c0,0.55-0.45,1-1,1h-2.17l4.93,4.93C21.91,18.65,22,18.34,22,18V6C22,4.9,21.1,4,20,4 z"></path><path d="M20,20l-6-6l-1.71-1.71L12,12L3.16,3.16c-0.39-0.39-1.02-0.39-1.41,0c-0.39,0.39-0.39,1.02,0,1.41l0.49,0.49 C2.09,5.35,2,5.66,2,6v12c0,1.1,0.9,2,2,2h13.17l2.25,2.25c0.39,0.39,1.02,0.39,1.41,0c0.39-0.39,0.39-1.02,0-1.41L20,20z M8,13 c0,0.55-0.45,1-1,1H5c-0.55,0-1-0.45-1-1c0-0.55,0.45-1,1-1h2C7.55,12,8,12.45,8,13z M14,17c0,0.55-0.45,1-1,1H5 c-0.55,0-1-0.45-1-1c0-0.55,0.45-1,1-1h8c0.08,0,0.14,0.03,0.21,0.04l0.74,0.74C13.97,16.86,14,16.92,14,17z"></path></g></g></symbol>
  <symbol id="media-subtitles" viewBox="0 0 24 24"><path d="M20 4H4c-1.1 0-2 .9-2 2v12c0 1.1.9 2 2 2h16c1.1 0 2-.9 2-2V6c0-1.1-.9-2-2-2zM5 12h2c.55 0 1 .45 1 1s-.45 1-1 1H5c-.55 0-1-.45-1-1s.45-1 1-1zm8 6H5c-.55 0-1-.45-1-1s.45-1 1-1h8c.55 0 1 .45 1 1s-.45 1-1 1zm6 0h-2c-.55 0-1-.45-1-1s.45-1 1-1h2c.55 0 1 .45 1 1s-.45 1-1 1zm0-4h-8c-.55 0-1-.45-1-1s.45-1 1-1h8c.55 0 1 .45 1 1s-.45 1-1 1z"></path></symbol>
  <symbol id="media-volume-off" viewBox="0 0 24 24"><path d="M3.63 3.63c-.39.39-.39 1.02 0 1.41L7.29 8.7 7 9H4c-.55 0-1 .45-1 1v4c0 .55.45 1 1 1h3l3.29 3.29c.63.63 1.71.18 1.71-.71v-4.17l4.18 4.18c-.49.37-1.02.68-1.6.91-.36.15-.58.53-.58.92 0 .72.73 1.18 1.39.91.8-.33 1.55-.77 2.22-1.31l1.34 1.34c.39.39 1.02.39 1.41 0 .39-.39.39-1.02 0-1.41L5.05 3.63c-.39-.39-1.02-.39-1.42 0zM19 12c0 .82-.15 1.61-.41 2.34l1.53 1.53c.56-1.17.88-2.48.88-3.87 0-3.83-2.4-7.11-5.78-8.4-.59-.23-1.22.23-1.22.86v.19c0 .38.25.71.61.85C17.18 6.54 19 9.06 19 12zm-8.71-6.29l-.17.17L12 7.76V6.41c0-.89-1.08-1.33-1.71-.7zM16.5 12c0-1.77-1.02-3.29-2.5-4.03v1.79l2.48 2.48c.01-.08.02-.16.02-.24z"></path></symbol>
  <symbol id="media-volume-up" viewBox="0 0 24 24"><path d="M3 10v4c0 .55.45 1 1 1h3l3.29 3.29c.63.63 1.71.18 1.71-.71V6.41c0-.89-1.08-1.34-1.71-.71L7 9H4c-.55 0-1 .45-1 1zm13.5 2c0-1.77-1.02-3.29-2.5-4.03v8.05c1.48-.73 2.5-2.25 2.5-4.02zM14 4.45v.2c0 .38.25.71.6.85C17.18 6.53 19 9.06 19 12s-1.82 5.47-4.4 6.5c-.36.14-.6.47-.6.85v.2c0 .63.63 1.07 1.21.85C18.6 19.11 21 15.84 21 12s-2.4-7.11-5.79-8.4c-.58-.23-1.21.22-1.21.85z"></path></symbol>
</svg>
`;

//  Parsing HTML O(1) global. cloneNode(true) O(1) par instanciation.
//  data-action = identifiant de commande. Résolution par InputSystem.
const _TEMPLATE = document.createElement("template");
_TEMPLATE.innerHTML = `
<div class="media-player">
  <button class="media-play-pause" data-action="TOGGLE_PLAY" aria-label="play/pause">
    <svg focusable="false"><use href="#media-play"></use></svg>
    <svg focusable="false"><use href="#media-play-disabled"></use></svg>
    <svg focusable="false"><use href="#media-pause"></use></svg>
  </button>
  <div class="media-tags">
    <output class="media-subtitle-langage"></output>
    <output class="media-playback-rate"></output>
  </div>
  <div class="media-time">
    <output class="media-current-time" aria-label="current time">0:00</output>
    &nbsp;/&nbsp;
    <output class="media-duration" aria-label="duration">0:00</output>
  </div>
  <input type="range" class="media-progress-bar" data-action="SEEK"
         aria-label="progress bar" min="0" max="100" step="1" value="0">
  <div class="media-extend-volume">
    <input type="range" class="media-volume-bar" data-action="SET_VOLUME"
           aria-label="volume bar" min="0" max="1" step=".1" value=".5">
    <button class="media-mute" data-action="MUTE" aria-label="mute">
      <svg focusable="false"><use href="#media-volume-up"></use></svg>
      <svg focusable="false"><use href="#media-volume-off"></use></svg>
    </button>
  </div>
  <button class="media-fullscreen" data-action="FULLSCREEN" aria-label="fullscreen">
    <svg focusable="false"><use href="#media-fullscreen"></use></svg>
  </button>
  <button class="media-menu" data-action="MENU" aria-label="menu">
    <svg focusable="false"><use href="#media-menu"></use></svg>
  </button>
  <div class="media-extend-menu">
    <button class="media-next-reading" data-action="NEXT_READING" aria-label="next reading mode">
      <svg focusable="false"><use href="#media-move-down"></use></svg>
    </button>
    <button class="media-subtitles" data-action="SUBTITLES" aria-label="subtitles">
      <svg focusable="false"><use href="#media-subtitles"></use></svg>
    </button>
    <button class="media-picture-in-picture" data-action="PIP" aria-label="picture in picture">
      <svg focusable="false"><use href="#media-picture-in-picture"></use></svg>
      <svg focusable="false"><use href="#media-picture-in-picture-alt"></use></svg>
    </button>
    <button class="media-slow-motion" data-action="SLOW_MOTION" aria-label="slow motion">
      <svg focusable="false"><use href="#media-slow-motion"></use></svg>
    </button>
    <button class="media-leap-rewind" data-action="LEAP_REWIND" aria-label="leap rewind">
      <svg focusable="false"><use href="#media-rewind-5"></use></svg>
    </button>
    <button class="media-leap-forward" data-action="LEAP_FORWARD" aria-label="leap forward">
      <svg focusable="false"><use href="#media-forward-5"></use></svg>
    </button>
    <button class="media-stop" data-action="STOP" aria-label="stop">
      <svg focusable="false"><use href="#media-stop"></use></svg>
    </button>
    <button class="media-replay" data-action="REPLAY" aria-label="replay">
      <svg focusable="false"><use href="#media-replay"></use></svg>
    </button>
  </div>
</div>`;

// ── 1. Constantes ─────────────────────────────────────────────────────────

const MEDIA_SELECTOR = ".media";
const PLAYBACK_RATES = Object.freeze([0.5, 0.25, 0.5, 1, 2, 4, 2, 1]);

// ── 2. Stores — Data Layout plat indexé par entityId (integer) ────────────

export const timeStore = {};
export const statusStore = {};
export const configStore = {};
export const domStore = {};
export const computedStore = {};

/** @type {WeakMap<HTMLMediaElement, number>} */
const _mediaIndex = new WeakMap();
let _nextEntityId = 0;

// ── 3. Command Buffer ──────────────────────────────────────────────────────

const _commandBuffer = [];

/** * Pousse une commande dans la file. Seul point d'écriture dans le buffer.
 * @param {number} entityId
 * @param {string} type
 * @param {any} [payload]
 */
export const dispatch = (entityId, type, payload) =>
	_commandBuffer.push({ entityId, type, payload });

// ── 4. Utilitaires purs ────────────────────────────────────────────────────

const _toTime = (s) => {
	// Check strict : évite toute tentative de cast si s venait à être corrompu
	if (!Number.isFinite(s) || s < 0) return "0:00";
	const hh = Math.floor(s / 3600);
	const mm = Math.floor((s % 3600) / 60).toString();
	const ss = Math.floor(s % 60)
		.toString()
		.padStart(2, "0");
	return hh > 0 ? `${hh}:${mm.padStart(2, "0")}:${ss}` : `${mm}:${ss}`;
};

const _cls = (el, name, on) => {
	if (el) on ? el.classList.add(name) : el.classList.remove(name);
};

const _closeOtherMenus = (currentId) => {
	for (const id in domStore) {
		const eid = +id;
		if (eid === currentId) continue;
		domStore[eid].extendMenu?.classList.remove("active");
		domStore[eid].menuButton?.classList.remove("active");
	}
};

// ── 5. CommandSystem ───────────────────────────────────────────────────────
//  Zéro DOM. Écrit uniquement dans configStore, les APIs natives et lève dirty.

const _handlers = {
	TOGGLE_PLAY(id) {
		const m = domStore[id].media;
		m.paused ? m.play() : m.pause();
		_closeOtherMenus(id);
		const nextId = configStore[id].nextEntityId;
		if (nextId !== null) {
			const nm = domStore[nextId]?.media;
			if (nm) nm.preload = "auto";
		}
	},

	SEEK(id, payload) {
		const m = domStore[id].media;
		if (!m.duration) return;
		m.currentTime = (payload.value / payload.max) * m.duration;
		computedStore[id].dirty = true;
	},

	SET_VOLUME(id, payload) {
		domStore[id].media.volume =
			parseFloat(payload.value) / parseFloat(payload.max);
		// Le DOM (volumeBar) sera mis à jour par UIRenderSystem après lecture de TimeSystem
		computedStore[id].dirty = true;
	},

	MUTE(id) {
		domStore[id].media.muted = !domStore[id].media.muted;
		computedStore[id].dirty = true;
	},

	STOP(id) {
		domStore[id].media.pause();
		domStore[id].media.currentTime = 0;
		computedStore[id].dirty = true;
	},

	REPLAY(id) {
		domStore[id].media.loop = !domStore[id].media.loop;
		computedStore[id].dirty = true;
	},

	LEAP_REWIND(id) {
		domStore[id].media.currentTime -= 5;
		computedStore[id].dirty = true;
	},

	LEAP_FORWARD(id) {
		domStore[id].media.currentTime += 5;
		computedStore[id].dirty = true;
	},

	SLOW_MOTION(id) {
		const cfg = configStore[id];
		cfg.playbackRateIdx = (cfg.playbackRateIdx + 1) % PLAYBACK_RATES.length;
		domStore[id].media.playbackRate = PLAYBACK_RATES[cfg.playbackRateIdx];
		computedStore[id].dirty = true;
	},

	SUBTITLES(id) {
		const cfg = configStore[id];
		const tracks = cfg.tracks;
		if (!tracks?.length) return;
		if (cfg.subtitleIdx >= 0 && tracks[cfg.subtitleIdx])
			tracks[cfg.subtitleIdx].mode = "disabled";

		const next = cfg.subtitleIdx + 1;
		if (next < tracks.length) {
			tracks[next].mode = "showing";
			cfg.subtitleIdx = next;
		} else {
			cfg.subtitleIdx = -1;
		}
		computedStore[id].dirty = true;
	},

	NEXT_READING(id) {
		const rel = configStore[id].mediaRelationship;
		if (!rel) return;
		const enabling = rel.dataset.nextReading !== "true";
		rel.dataset.nextReading = enabling ? "true" : "false";

		for (const oid in configStore) {
			if (configStore[+oid].mediaRelationship !== rel) continue;
			if (enabling) domStore[+oid].media.loop = false;
			computedStore[+oid].dirty = true; // force refresh de nextReadingButton
		}
	},

	FULLSCREEN(id) {
		domStore[id].media.requestFullscreen?.();
	},

	PIP(id) {
		if (document.pictureInPictureElement) {
			document.exitPictureInPicture();
		} else if (document.pictureInPictureEnabled) {
			domStore[id].media.requestPictureInPicture().catch(() => {});
		}
	},

	MENU(id) {
		const dom = domStore[id];
		if (!dom.extendMenu) return;
		dom.extendMenu.classList.toggle("active");
		dom.menuButton.classList.toggle("active");
		_closeOtherMenus(id);
	},

	_ADVANCE(id) {
		MediaStackSystem.advance(id);
	},

	_STREAM_DETECTED(id) {
		configStore[id].isStream = true;
		computedStore[id].dirty = true;
	},

	_ERROR(id) {
		statusStore[id].error = true;
		computedStore[id].dirty = true;
	},
};

const CommandSystem = {
	run() {
		const len = _commandBuffer.length;
		for (let i = 0; i < len; i++) {
			const { entityId, type, payload } = _commandBuffer[i];
			_handlers[type]?.(entityId, payload);
		}
		_commandBuffer.splice(0, len);
	},
};

// ── 6. TimeSystem ──────────────────────────────────────────────────────────

const TimeSystem = {
	run() {
		for (const id in domStore) {
			const eid = +id;
			const m = domStore[eid].media;
			const ts = timeStore[eid];
			const ss = statusStore[eid];

			const prevTime = ts.currentTime;
			const prevDuration = ts.duration;
			const prevBuffered = ts.bufferedEnd;
			const prevVolume = ss.volume;
			const prevRate = ss.playbackRate;

			ts.currentTime = m.currentTime;
			ts.duration = m.duration;
			ts.bufferedEnd =
				m.buffered.length > 0 ? m.buffered.end(m.buffered.length - 1) : 0;

			ss.paused = m.paused;
			ss.muted = m.muted;
			ss.volume = m.volume;
			ss.loop = m.loop;
			ss.playbackRate = m.playbackRate;

			if (
				ts.currentTime !== prevTime ||
				ts.duration !== prevDuration ||
				ts.bufferedEnd !== prevBuffered ||
				ss.volume !== prevVolume ||
				ss.playbackRate !== prevRate
			) {
				computedStore[eid].dirty = true;
			}
		}
	},
};

// ── 7. LogicSystem ─────────────────────────────────────────────────────────

const LogicSystem = {
	run() {
		for (const id in timeStore) {
			const eid = +id;
			const cs = computedStore[eid];
			if (!cs.dirty) continue;

			const ts = timeStore[eid];
			const ss = statusStore[eid];
			const cfg = configStore[eid];
			const dur = ts.duration;

			cs.ratio = dur > 0 ? Math.floor((ts.currentTime / dur) * 1000) / 10 : 0;
			cs.bufferRatio = dur > 0 ? Math.floor((ts.bufferedEnd / dur) * 100) : 0;
			cs.timeStr = _toTime(ts.currentTime);
			cs.durationStr = _toTime(dur);
			cs.isPlaying = !ss.paused;
			cs.isMuted = ss.muted || ss.volume === 0;
			cs.isStopped = ss.paused && ts.currentTime === 0;

			// Extraction des états discrets nécessitant une mise à jour UI
			if (cfg.subtitleIdx >= 0 && cfg.tracks[cfg.subtitleIdx]) {
				cs.subtitleStr = `cc: ${cfg.tracks[cfg.subtitleIdx].language}`;
				cs.hasSubtitles = true;
			} else {
				cs.subtitleStr = "";
				cs.hasSubtitles = false;
			}

			cs.isNextReading = cfg.mediaRelationship?.dataset.nextReading === "true";
			cs.isPip = document.pictureInPictureElement === domStore[eid].media;
		}
	},
};

// ── 8. UIRenderSystem ──────────────────────────────────────────────────────
//  Unique point d'écriture DOM (INV-1).
//  Prend en charge les mutations structurelles uniques via nullification de références.

const UIRenderSystem = {
	run() {
		for (const id in computedStore) {
			const eid = +id;
			const cs = computedStore[eid];
			if (!cs.intersecting || !cs.dirty) continue;

			const dom = domStore[eid];
			const ss = statusStore[eid];
			const cfg = configStore[eid];

			// ── Mutations topologiques uniques (Structural Changes) ──

			if (cfg.isStream && dom.progressBar) {
				const timeEl = dom.player.querySelector(".media-time");
				if (timeEl) {
					timeEl.textContent = "Lecture en continu";
					timeEl.style.marginRight = "auto";
				}
				dom.progressBar.remove();
				dom.progressBar = null;
				dom.menuButton?.remove();
				dom.menuButton = null;
				dom.extendMenu?.remove();
				dom.extendMenu = null;
			}

			if (ss.error && !dom.player.hasAttribute("inert")) {
				dom.player.setAttribute("inert", "");

				// Remplacement du .forEach par un parcours indexé O(N) direct sans allocation.
				const controls = dom.player.querySelectorAll("button, input");
				for (let i = 0; i < controls.length; i++) {
					controls[i].disabled = true;
				}

				dom.media.classList.add("error");
				dom.player.classList.add("error");
				dom.player.querySelector(".media-time").textContent =
					"Erreur de lecture";
				if ("poster" in dom.media) dom.media.poster = "";
			}

			// ── Mises à jour cycliques ──

			if (dom.progressBar) {
				dom.progressBar.value = cs.ratio;
				dom.progressBar.style.setProperty("--position", `${cs.ratio}%`);
				dom.progressBar.style.setProperty(
					"--position-buffer",
					`${cs.bufferRatio}%`,
				);
			}

			if (dom.volumeBar) {
				dom.volumeBar.style.setProperty("--position", `${ss.volume * 100}%`);
			}

			if (
				dom.playbackRateOutput &&
				dom.playbackRateOutput.textContent !== `x${ss.playbackRate}`
			) {
				dom.playbackRateOutput.textContent = `x${ss.playbackRate}`;
				_cls(dom.playbackRateOutput, "active", ss.playbackRate !== 1);
				_cls(dom.slowMotionButton, "active", ss.playbackRate !== 1);
			}

			if (
				dom.subtitleLangageOutput &&
				dom.subtitleLangageOutput.value !== cs.subtitleStr
			) {
				dom.subtitleLangageOutput.value = cs.subtitleStr;
				_cls(dom.subtitlesButton, "active", cs.hasSubtitles);
				_cls(dom.subtitleLangageOutput, "active", cs.hasSubtitles);
			}

			dom.currentTimeOutput.value = cs.timeStr;

			if (dom.durationOutput.value !== cs.durationStr) {
				dom.durationOutput.value = cs.durationStr;
			}

			_cls(dom.playPauseButton, "active", cs.isPlaying);
			_cls(dom.muteButton, "active", cs.isMuted);
			_cls(dom.nextReadingButton, "active", cs.isNextReading);
			_cls(dom.pipButton, "active", cs.isPip);

			if (dom.stopButton) {
				_cls(dom.stopButton, "active", cs.isStopped);
				dom.stopButton.disabled = cs.isStopped;
			}

			if (dom.replayButton) _cls(dom.replayButton, "active", ss.loop);

			cs.dirty = false;
		}
	},
};

// ── 9. MediaStackSystem ────────────────────────────────────────────────────

const MediaStackSystem = {
	advance(id) {
		const cfg = configStore[id];
		if (!cfg.mediaRelationship) return;
		if (cfg.mediaRelationship.dataset.nextReading !== "true") return;

		let candidateId = cfg.nextEntityId;
		while (candidateId !== null && statusStore[candidateId]?.error) {
			candidateId = configStore[candidateId]?.nextEntityId ?? null;
		}
		if (candidateId === null || candidateId === id) return;

		domStore[candidateId].media.play();

		const nextNextId = configStore[candidateId]?.nextEntityId ?? null;
		if (nextNextId !== null) {
			const m = domStore[nextNextId]?.media;
			if (m) m.preload = "auto";
		}
	},
};

// ── 10. IntersectionObserver ───────────────────────────────────────────────

const _observer = new IntersectionObserver(
	(entries) => {
		for (const entry of entries) {
			const id = +entry.target.dataset.entityId;
			if (computedStore[id])
				computedStore[id].intersecting = entry.isIntersecting;
		}
	},
	{ threshold: 0.1 },
);

// ── 11. InputSystem ────────────────────────────────────────────────────────

const InputSystem = {
	_ac: null,
	_initialized: false,

	init() {
		if (this._initialized) return;
		this._initialized = true;

		const ac = new AbortController();
		this._ac = ac;
		const sig = ac.signal;

		document.addEventListener("click", this._route, { signal: sig });
		document.addEventListener("input", this._route, { signal: sig });

		document.addEventListener(
			"play",
			(e) => {
				const srcId = _mediaIndex.get(e.target);
				if (srcId === undefined) return;
				for (const id in domStore) {
					const eid = +id;
					if (eid !== srcId) domStore[eid].media.pause();
				}
			},
			{ signal: sig, capture: true },
		);

		document.addEventListener(
			"fullscreenchange",
			() => {
				const active = !!document.fullscreenElement;
				for (const id in domStore)
					_cls(domStore[+id].fullscreenButton, "active", active);
			},
			{ signal: sig },
		);
	},

	_route(e) {
		const el = e.target.closest("[data-action]");
		if (!el) return;
		const playerEl = el.closest("[data-entity-id]");
		if (!playerEl) return;
		dispatch(
			+playerEl.dataset.entityId,
			el.dataset.action,
			e.target.type === "range"
				? { value: e.target.value, max: e.target.max }
				: undefined,
		);
	},

	dispose() {
		this._ac?.abort();
		this._initialized = false;
	},
};

// ── 12. Engine ─────────────────────────────────────────────────────────────

const Engine = {
	_rafId: null,
	_running: false,

	tick() {
		CommandSystem.run();
		TimeSystem.run();
		LogicSystem.run();
		UIRenderSystem.run();
		Engine._rafId = requestAnimationFrame(Engine.tick);
	},

	start() {
		if (this._running) return;
		this._running = true;
		this._rafId = requestAnimationFrame(Engine.tick);
	},

	stop() {
		cancelAnimationFrame(this._rafId);
		this._running = false;
	},
};

// ── 13. Initialisation d'une entité ────────────────────────────────────────

const _initEntity = (media, entityId) => {
	const player = _TEMPLATE.content
		.cloneNode(true)
		.querySelector(".media-player");
	player.dataset.entityId = entityId;
	media.insertAdjacentElement("afterend", player);

	const q = (sel) => player.querySelector(sel);
	domStore[entityId] = {
		media,
		player,
		playPauseButton: q(".media-play-pause"),
		playbackRateOutput: q(".media-playback-rate"),
		subtitleLangageOutput: q(".media-subtitle-langage"),
		currentTimeOutput: q(".media-current-time"),
		durationOutput: q(".media-duration"),
		progressBar: q(".media-progress-bar"),
		volumeBar: q(".media-volume-bar"),
		muteButton: q(".media-mute"),
		fullscreenButton: q(".media-fullscreen"),
		menuButton: q(".media-menu"),
		extendMenu: q(".media-extend-menu"),
		nextReadingButton: q(".media-next-reading"),
		subtitlesButton: q(".media-subtitles"),
		pipButton: q(".media-picture-in-picture"),
		slowMotionButton: q(".media-slow-motion"),
		stopButton: q(".media-stop"),
		replayButton: q(".media-replay"),
	};

	timeStore[entityId] = { currentTime: 0, duration: NaN, bufferedEnd: 0 };

	statusStore[entityId] = {
		paused: true,
		muted: false,
		volume: 0.5,
		loop: false,
		playbackRate: 1,
		waiting: false,
		error: false,
	};

	const mediaRelationship = media.closest(".media-relationship");
	const ac = new AbortController();

	configStore[entityId] = {
		isAudio: media.tagName === "AUDIO",
		isStream: false,
		tracks: media.textTracks,
		subtitleIdx: -1,
		playbackRateIdx: 0,
		mediaRelationship,
		nextEntityId: null,
		nextNextEntityId: null,
		_ac: ac,
	};

	computedStore[entityId] = {
		ratio: 0,
		bufferRatio: 0,
		timeStr: "0:00",
		durationStr: "0:00",
		subtitleStr: "",
		hasSubtitles: false,
		isPlaying: false,
		isMuted: false,
		isStopped: true,
		isNextReading: false,
		isPip: false,
		dirty: true,
		intersecting: true,
	};

	_mediaIndex.set(media, entityId);

	const dom = domStore[entityId];
	const isAudio = media.tagName === "AUDIO";

	if (isAudio || !document.fullscreenEnabled) {
		dom.fullscreenButton?.remove();
		dom.fullscreenButton = null;
	}
	if (isAudio || !document.pictureInPictureEnabled) {
		dom.pipButton?.remove();
		dom.pipButton = null;
	}
	if (!media.textTracks[0]) {
		dom.subtitlesButton?.remove();
		dom.subtitlesButton = null;
	}
	if (!mediaRelationship) {
		dom.nextReadingButton?.remove();
		dom.nextReadingButton = null;
	}

	dom.progressBar.style.setProperty("--position", "0%");
	dom.progressBar.style.setProperty("--position-buffer", "0%");
	dom.volumeBar.style.setProperty("--position", "50%");

	const sig = ac.signal;

	const _setDuration = () => {
		// Validation stricte du Float64 renvoyé par l'API HTMLMediaElement
		if (Number.isFinite(media.duration)) computedStore[entityId].dirty = true;
	};

	media.readyState >= 1
		? _setDuration()
		: media.addEventListener("loadedmetadata", _setDuration, {
				signal: sig,
				once: true,
			});

	const _handleInfinity = () => {
		if (media.duration !== Infinity) return;
		dispatch(entityId, "_STREAM_DETECTED");

		// Unrolling : zéro allocation, pas de closure, pas de tableau éphémère.
		media.removeEventListener("loadeddata", _handleInfinity);
		media.removeEventListener("loadedmetadata", _handleInfinity);
		media.removeEventListener("play", _handleInfinity);
	};

	media.addEventListener("loadeddata", _handleInfinity, { signal: sig });
	media.addEventListener("loadedmetadata", _handleInfinity, { signal: sig });
	media.addEventListener("play", _handleInfinity, { signal: sig });

	media.addEventListener(
		"waiting",
		() => {
			statusStore[entityId].waiting = true;
			player.classList.add("waiting");
		},
		{ signal: sig },
	);

	media.addEventListener(
		"canplay",
		() => {
			statusStore[entityId].waiting = false;
			player.classList.remove("waiting");
			if (mediaRelationship) {
				computedStore[entityId].dirty = true;
			}
		},
		{ signal: sig },
	);

	media.addEventListener(
		"ended",
		() => {
			media.currentTime = 0;
			computedStore[entityId].dirty = true;
			dispatch(entityId, "_ADVANCE");
			const nn = configStore[entityId].nextNextEntityId;
			if (nn !== null) {
				const m = domStore[nn]?.media;
				if (m) m.preload = "auto";
			}
		},
		{ signal: sig },
	);

	if (dom.subtitlesButton) {
		for (let i = 0; i < media.textTracks.length; i++) {
			if (media.textTracks[i].mode !== "showing") continue;
			configStore[entityId].subtitleIdx = i;
			break;
		}
	}

	media.src = media.currentSrc;
	media.addEventListener("error", () => dispatch(entityId, "_ERROR"), {
		signal: sig,
		capture: true,
	});

	_observer.observe(player);
};

// ── 14. Résolution des adjacences cross-entités ────────────────────────────

const _resolveAdjacencies = () => {
	for (const id in configStore) {
		const eid = +id;
		const rel = configStore[eid].mediaRelationship;
		if (!rel) continue;
		const siblings = [...rel.querySelectorAll(MEDIA_SELECTOR)];
		const idx = siblings.indexOf(domStore[eid].media);
		const nextM = siblings[idx + 1] ?? siblings[0] ?? null;
		const nextNM = siblings[idx + 2] ?? siblings[0] ?? null;
		const nextId = nextM ? (_mediaIndex.get(nextM) ?? null) : null;
		const nextNId = nextNM ? (_mediaIndex.get(nextNM) ?? null) : null;
		configStore[eid].nextEntityId = nextId === eid ? null : nextId;
		configStore[eid].nextNextEntityId = nextNId === eid ? null : nextNId;
	}
};

// ── 15. Export API ─────────────────────────────────────────────────────────

export const disposeEntity = (entityId) => {
	configStore[entityId]?._ac.abort();
	_observer.unobserve(domStore[entityId]?.player);
	domStore[entityId]?.player.remove();
	_mediaIndex.delete(domStore[entityId]?.media);

	// Note DOD: Le mot clé 'delete' dé-optimise le dictionnaire V8.
	// Acceptable ici car utilisé occasionnellement pour la libération de la GC.
	delete timeStore[entityId];
	delete statusStore[entityId];
	delete configStore[entityId];
	delete computedStore[entityId];
	delete domStore[entityId];
};

/**
 * Initialise le lecteur.
 * @param {HTMLElement|Document} container
 */
export const bootstrap = (container = document) => {
	// 1. Initialisation unique du layout de données statiques (Sprites)
	// TRADE-OFF DOD : L'état d'injection est vérifié via le DOM (I/O) plutôt
	// que par un flag en RAM.
	// Justification : Le cycle de vie des vues (ex: View Transitions API / page
	// swap) peut détruire physiquement ce nœud lors du remplacement du layout.
	// Ce lookup garantit l'auto-réparation et la ré-injection post-navigation.
	if (!document.getElementById("media-sprites")) {
		document.body.insertAdjacentHTML("afterbegin", _SVG_SPRITES);
	}

	// 2. Traitement des entités (Instances)
	const medias = container.querySelectorAll(MEDIA_SELECTOR);
	for (const media of medias) {
		if (_mediaIndex.has(media)) continue;
		media.removeAttribute("controls");
		media.id = media.id || `media-${_nextEntityId}`;
		_initEntity(media, _nextEntityId);
		_nextEntityId++;
	}
	_resolveAdjacencies();
	InputSystem.init();
	Engine.start();
};

// Compatibilité pour l'exécution automatique si le module est importé
// de façon synchrone dans un document déjà chargé.
if (document.readyState === "loading") {
	document.addEventListener("DOMContentLoaded", () => bootstrap(), {
		once: true,
	});
} else {
	// Optionnel : Vous pouvez retirer cet appel automatique si vous
	// préférez maîtriser strictement l'amorçage via l'import dans votre main.js
	bootstrap();
}

export const stores = {
	time: timeStore,
	status: statusStore,
	config: configStore,
	computed: computedStore,
};
