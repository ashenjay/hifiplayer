import { getCurrentWindow } from '@tauri-apps/api/window'
import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import App from './App.tsx'
import { inTauri } from './api'
import { MiniPlayer } from './components/Extras'
import './styles.css'

// The mini player is a second window running the same page.
const isMini = inTauri ? getCurrentWindow().label === 'mini' : location.hash === '#mini'

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    {isMini ? <MiniPlayer /> : <App />}
  </StrictMode>,
)
