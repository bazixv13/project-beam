import { useEffect, useState } from 'react';

export default function WaterEffect() {
  const [ripples, setRipples] = useState([]);

  useEffect(() => {
    let lastTime = 0;
    
    const handleMouseMove = (e) => {
      const now = Date.now();
      // Throttle ripple creation to prevent performance issues
      if (now - lastTime > 80) {
        lastTime = now;
        
        const newRipple = {
          id: now,
          x: e.clientX,
          y: e.clientY
        };
        
        setRipples(prev => [...prev, newRipple]);
        
        // Remove ripple after animation finishes
        setTimeout(() => {
          setRipples(prev => prev.filter(r => r.id !== newRipple.id));
        }, 2500);
      }
    };

    window.addEventListener('mousemove', handleMouseMove);
    return () => window.removeEventListener('mousemove', handleMouseMove);
  }, []);

  return (
    <div className="water-container">
      {ripples.map(r => (
        <div 
          key={r.id} 
          className="water-ripple"
          style={{ left: r.x, top: r.y }}
        ></div>
      ))}
    </div>
  );
}
