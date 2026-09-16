export function ProfileCard({ person, onPick }: CardProps) {
    const displayName = person.first + " " + person.last;
    const isHot = person.score > 100;
    return (
        <div className="card">
            <span>{displayName}</span>
            <button onClick={() => onPick(person.id)}>{isHot ? "hot" : "cold"}</button>
        </div>
    );
}
