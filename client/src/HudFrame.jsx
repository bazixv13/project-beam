import React from 'react';

export default function HudFrame({ children }) {
  return (
    <div className="hud-frame-wrapper">
      {/* SVG Border Frame */}
      <svg className="hud-frame-svg" viewBox="0 0 500 600" preserveAspectRatio="none" fill="none" xmlns="http://www.w3.org/2000/svg">
        {/* Main frame path */}
        <path 
          d="
            M 40 2
            L 430 2
            L 460 2
            L 498 2
            L 498 30
            L 498 40
            L 498 560
            L 460 598
            L 40 598
            L 2 598
            L 2 560
            L 2 40
            Z
          " 
          stroke="white" 
          strokeWidth="2" 
          fill="none"
        />
        
        {/* Top-left corner cut decoration */}
        <path d="M 2 40 L 30 2" stroke="white" strokeWidth="2" />
        
        {/* Bottom-right corner cut */}
        <path d="M 460 598 L 498 560" stroke="white" strokeWidth="2" />
        
        {/* Top-right notch step */}
        <path 
          d="M 430 2 L 430 16 L 460 16 L 460 2" 
          stroke="white" 
          strokeWidth="2" 
          fill="none"
        />
        
        {/* Hash marks - top left */}
        <line x1="12" y1="50" x2="28" y2="36" stroke="white" strokeWidth="1.5" opacity="0.7" />
        <line x1="12" y1="58" x2="36" y2="36" stroke="white" strokeWidth="1.5" opacity="0.7" />
        <line x1="12" y1="66" x2="44" y2="36" stroke="white" strokeWidth="1.5" opacity="0.5" />
        <line x1="16" y1="70" x2="48" y2="40" stroke="white" strokeWidth="1.5" opacity="0.3" />

        {/* Small decorative dots */}
        <circle cx="490" cy="20" r="3" fill="white" opacity="0.5" />
        <circle cx="480" cy="20" r="1.5" fill="white" opacity="0.3" />
        
        {/* Bottom-left small lines */}
        <line x1="8" y1="565" x2="8" y2="580" stroke="white" strokeWidth="1" opacity="0.3" />
        <line x1="14" y1="570" x2="14" y2="590" stroke="white" strokeWidth="1" opacity="0.2" />
      </svg>

      {/* Content inside the frame */}
      <div className="hud-frame-content">
        {children}
      </div>
    </div>
  );
}
