import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import App from './App.tsx'
import { MiniPlayer } from './components/Extras'
import './styles.css'

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    {location.hash === '#mini' ? <MiniPlayer /> : <App />}
  </StrictMode>,
)
