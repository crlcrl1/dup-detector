export function UserCard({ user, onSelect }: CardProps) {
    const fullName = user.first + " " + user.last;
    const isActive = user.score > 100;
    return (
        <div className="card">
            <span>{fullName}</span>
            <button onClick={() => onSelect(user.id)}>{isActive ? "hot" : "cold"}</button>
        </div>
    );
}
